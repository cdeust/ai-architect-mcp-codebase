// parser::spec::walkers::clike — the flat C-family definition walker
// (ADR-0055 phase 6). Reproduces the pre-migration hand-written C walker
// (`parser::c::extract`) at exact parity, driven by the `CFamilySpec` sub-table
// instead of hardcoded `TS_*` constants.
//
// C-family grammars are flat, not class-recursive: a file is a sequence of
// struct/union/enum/typedef/function/prototype/#include, and structs carry
// FIELDS rather than methods. `walk_defs` delegates here whenever a `LangSpec`
// carries `c_family: Some(_)`, so the class-model arms (emit_class, receiver
// methods, variants) stay untouched. Calls and imports still route through the
// SHARED generic walkers (`calls::walk_calls`, `imports::walk_imports`) via the
// conventions — only the definition shapes are C-specific.
//
// What this walker is NOT is the whole C family. C++ (phase 7) needs namespaces,
// class-scoped methods, inheritance, and a single per-file `seq` ordering that a
// flat walker has no model for, so it rides a sibling walker (`walkers/cpp`) on
// its own `CppFamilySpec` row. The dedup ADR-0055 asks for is realized at the
// MECHANISM instead: the name search both walkers use lives once in
// `walkers/declarator`, driven by the `DeclaratorNaming` sub-table both family
// rows carry — so the #106 "name from the declarator, never a parameter" fix is
// inherited by C++ as DATA (#123), not copied. ObjC (phase 8) joins whichever of
// the two structural models its grammar actually matches.

use tree_sitter::Node;

use super::super::lang_spec::{CFamilySpec, LangSpec};
use super::declarator::{
    binds_function_prototype, declarator_field_children, declarator_name, first_identifier,
    named_or_first_identifier,
};
use super::{calls, end_line_of, imports, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_ENUM,
    LABEL_FIELD, LABEL_FUNCTION, LABEL_STRUCT,
};

/// Flat C-family definition walker: dispatches each child of `parent` to the
/// concern its node kind names in `cf`, recursing transparently through any
/// unmatched wrapper node that has named children (preprocessor conditionals
/// `#ifdef … #endif`, which hold declarations the graph must still see). The
/// scope is unchanged across the recursion — C is flat, so a struct or function
/// inside an `#ifdef` is still a top-level (file-scoped) definition.
pub(super) fn walk_c_defs(
    spec: &LangSpec,
    cf: &CFamilySpec,
    ctx: &mut WalkCtx,
    parent: Node,
    scope: &str,
) {
    walk_c_items(CWalk { spec, cf }, ctx, parent, scope);
    // Linkage is per identifier in the file (issue #400): a function declared
    // `static` once stays internal where a later declaration omits the keyword.
    super::super::c_family::propagate_internal_linkage(&mut ctx.nodes);
}

/// The recursive body of `walk_c_defs`: one pass over `parent`'s children.
fn walk_c_items(w: CWalk, ctx: &mut WalkCtx, parent: Node, scope: &str) {
    let CWalk { spec, cf } = w;
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if kind_in(cf.struct_like_kinds, k) {
            emit_struct(w, ctx, child, scope);
        } else if kind_in(cf.enum_like_kinds, k) {
            emit_enum(w, ctx, child, scope);
        } else if kind_in(cf.typedef_kinds, k) {
            emit_typedef(w, ctx, child, scope);
        } else if kind_in(cf.func_def_kinds, k) {
            emit_function(w, ctx, child, scope);
        } else if kind_in(spec.import_node_kinds, k) {
            imports::walk_imports(spec, ctx, child, scope);
        } else if kind_in(cf.macro_object_kinds, k) || kind_in(cf.macro_function_kinds, k) {
            emit_macro(w, ctx, child, scope);
        } else if kind_in(cf.func_decl_kinds, k) {
            // `declaration` is shared by prototypes (`int f(void);`) and plain
            // variable declarations (`int x;`); only the former — carrying a
            // function declarator — is emitted, matching the hand-written walker.
            if is_c_function_prototype(cf, child) {
                emit_prototype(w, ctx, child, scope);
            } else {
                // Not a prototype, but `struct Foo { int x; } var;` still
                // declares a type inline (issue #107). The variable itself is
                // not a graph node (C locals/globals are out of scope for the
                // flat walker), the TYPE is.
                emit_inline_type(w, ctx, child, Placement { scope, alias: None });
            }
        } else if child.named_child_count() > 0 {
            // Transparent recursion into an unmatched wrapper with named
            // children (preprocessor conditionals, and any grammar wrapper the
            // hand-written `extract_top`'s `_ =>` arm descended). Same scope.
            // mutation note (§12): the `> 0` guard's `> 0` → `>= 0` mutant is a
            // proven EQUIVALENT mutant — recursing into a childless node is a
            // no-op (the child loop iterates nothing and no node is emitted on
            // entry), so no test can observe a difference. The guard is kept as a
            // faithful copy of the old walker's `named_child_count() > 0` and a
            // cheap skip of leaf recursion.
            walk_c_items(w, ctx, child, scope);
        }
    }
}

/// The language spec and its C-family table, passed together to every emitter
/// (coding-standards §4.4, at most four parameters; the checklist item in
/// .github/PULL_REQUEST_TEMPLATE.md).
#[derive(Clone, Copy)]
struct CWalk<'s> {
    spec: &'s LangSpec,
    cf: &'s CFamilySpec,
}

/// Where a type is emitted, and the name an ANONYMOUS specifier takes there
/// (the typedef alias of `typedef struct { … } T;`); `None` keeps the
/// specifier's own name.
#[derive(Clone, Copy)]
struct Placement<'a> {
    scope: &'a str,
    alias: Option<&'a str>,
}

/// The first `field_identifier` leaf found in a right-to-left DFS of `node`,
/// unwrapping pointer/array/function declarators to the bare field name
/// (`int *p` → `p`, `char buf[8]` → `buf`, `int (*h)(int)` → `h`). Same LIFO-DFS
/// order as `declarator::first_identifier`, but keyed on the single
/// `field_identifier_kind` rather than the `identifier_kinds` set — reproduces
/// the hand-written `find_field_identifier`.
fn find_field_identifier(cf: &CFamilySpec, source: &str, node: Node) -> String {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == cf.field_identifier_kind {
            return node_text(source, n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    String::new()
}

/// A `func_decl_kinds` node is a function prototype iff its declarator binds a
/// callable — a `function_declarator` reached with no pointer/reference wrapper
/// between it and the name.
///
/// The declaration's `declarator_field` child is a `function_declarator`
/// (`int f(void);`), an `init_declarator` (`int f(void) = …;`, `int x = 5;`),
/// a plain identifier (`int x;`), or a function-POINTER declarator
/// (`int (*signal_handler)(int) = 0;`). `binds_function_prototype` follows that
/// chain and answers `true` only for a real prototype: a plain variable is not a
/// prototype, and — the #135 C analog — neither is a function-pointer variable
/// (a `pointer_declarator` sits between the `function_declarator` and the name,
/// so it is data; the flat C walker does not model file-scope variables, so it
/// emits NOTHING for it rather than a bogus `Function`).
fn is_c_function_prototype(cf: &CFamilySpec, node: Node) -> bool {
    node.child_by_field_name(cf.naming.declarator_field)
        .map(|d| binds_function_prototype(cf.naming, cf.func_declarator_kind, d))
        .unwrap_or(false)
}

/// Emits a struct/union (`Struct` + `Defines`) and, from its `body_field`, one
/// `Field` + `HasField` per declared member (declarators unwrapped to their
/// field name; a member with no field name — an anonymous member — is skipped).
fn emit_struct(w: CWalk, ctx: &mut WalkCtx, node: Node, scope: &str) {
    emit_struct_named(w, ctx, node, Placement { scope, alias: None });
}

/// `emit_struct` with an optional name override for an ANONYMOUS specifier.
///
/// `typedef struct { int x; } T;` declares a type whose only usable name is the
/// typedef alias. Without the override the specifier has no `name` field, the
/// identifier fallback finds nothing (its members are `field_identifier`, not
/// `identifier`), and the whole struct — fields included — is dropped, which is
/// the second half of issue #107.
fn emit_struct_named(w: CWalk, ctx: &mut WalkCtx, node: Node, at: Placement) {
    let CWalk { spec, cf } = w;
    let scope = at.scope;
    let name = match at.alias {
        Some(n) => n.to_string(),
        None => named_or_first_identifier(cf.naming, spec, ctx.source, node),
    };
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        // C has no access keyword — `visibility_of` returns `public` for every
        // name; routing through it keeps that choice observable (a parity test
        // pins the emitted `public`), matching the hand-written walker.
        visibility: spec.conventions.visibility_of(&name),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        emit_struct_fields(w, ctx, body, &qn);
    }
}

/// Emits one `Field` + `HasField` per declared name in each `field_decl_kinds`
/// member of `body`. A single `field_declaration` may declare several names
/// (`int a, b, c;`), so every `declarator_field` child is emitted; each is
/// unwrapped (pointer/array/function) to its `field_identifier`. The type
/// annotation is the shared `type_field` text.
fn emit_struct_fields(w: CWalk, ctx: &mut WalkCtx, body: Node, owner_qn: &str) {
    let CWalk { spec, cf } = w;
    let mut bc = body.walk();
    for fd in body.children(&mut bc) {
        if !kind_in(cf.field_decl_kinds, fd.kind()) {
            continue;
        }
        let type_text = node_field_text(ctx.source, fd, spec.type_field);
        for declarator in declarator_field_children(cf.naming, fd) {
            let fname = find_field_identifier(cf, ctx.source, declarator);
            if fname.is_empty() {
                continue;
            }
            let fqn = qual(owner_qn, &fname);
            let mut props = Vec::new();
            if !type_text.is_empty() {
                props.push(("type_annotation".to_string(), type_text.clone()));
            }
            ctx.nodes.push(ExtractedNode {
                label: LABEL_FIELD.to_string(),
                name: fname.clone(),
                qualified_name: fqn.clone(),
                start_line: line_of(fd),
                end_line: end_line_of(fd),
                visibility: spec.conventions.visibility_of(&fname),
                properties: props,
            });
            ctx.refs.push(ExtractedRef {
                kind: "HasField".to_string(),
                from_qualified_name: owner_qn.to_string(),
                to_qualified_name: fqn,
            });
        }
    }
}

/// Emits an enum (`Enum` + `Defines`) and, from its `body_field`, one `Constant`
/// (`enum_entry=true`) + `Defines` per `enum_member_kinds` entry, scoped under
/// the enum. An entry with a value (`GREEN = 5`) still resolves to its name — the
/// value literal is not an identifier leaf.
fn emit_enum(w: CWalk, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let CWalk { spec, cf } = w;
    let name = named_or_first_identifier(cf.naming, spec, ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_ENUM.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    let body = match spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        Some(b) => b,
        None => return,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if !kind_in(cf.enum_member_kinds, child.kind()) {
            continue;
        }
        let en = first_identifier(cf.naming, ctx.source, child);
        if en.is_empty() {
            continue;
        }
        let eqn = qual(&qn, &en);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_CONSTANT.to_string(),
            name: en.clone(),
            qualified_name: eqn.clone(),
            start_line: line_of(child),
            end_line: end_line_of(child),
            visibility: spec.conventions.visibility_of(&en),
            properties: vec![("enum_entry".to_string(), "true".to_string())],
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: qn.clone(),
            to_qualified_name: eqn,
        });
    }
}

/// Emits a preprocessor macro (issue #107).
///
/// `label` splits the two shapes the graph must distinguish: an object-like
/// `#define MAX 10` is a value (`Constant`), a function-like
/// `#define SQUARE(x) ((x)*(x))` is callable (`Function`). Both carry
/// `macro=true` so a consumer can tell a macro from a real declaration — the
/// preprocessor runs before the compiler, so a macro is not a C object and
/// silently presenting it as one would be its own defect.
///
/// No body is scanned for calls: a macro's replacement list is unexpanded
/// tokens, not an expression the graph can attribute call sites to. Emitting
/// speculative `Calls` edges from a macro body would be inventing edges the
/// grammar does not support.
fn emit_macro(w: CWalk, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let CWalk { spec, cf } = w;
    // A macro taking arguments is called like a function; any other names a value.
    let label = if kind_in(cf.macro_function_kinds, node.kind()) {
        LABEL_FUNCTION
    } else {
        LABEL_CONSTANT
    };
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: super::super::c_family::macro_props(label),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits a struct/union/enum body declared INLINE inside another declaration
/// (issue #107): `typedef struct { int x; } T;` and
/// `struct Foo { int x; } var;` both carry the specifier in their `type` field,
/// which the flat walker's top-level scan never reaches.
///
/// Emitting the inner specifier here is what makes its FIELDS visible — before
/// this, a typedef'd struct contributed a `Constant` for the alias and nothing
/// for its members.
/// What `emit_inline_type` found, so the caller knows whether the alias name has
/// already been consumed by an anonymous type.
#[derive(PartialEq, Eq)]
enum InlineType {
    /// No inline DEFINITION (absent, or a bare reference like `struct Point`).
    None,
    /// A named inline definition (`typedef struct Tag { … } T;`).
    Named,
    /// An anonymous inline definition, emitted under `alias`.
    Anonymous,
}

fn emit_inline_type(w: CWalk, ctx: &mut WalkCtx, node: Node, at: Placement) -> InlineType {
    let CWalk { spec, cf } = w;
    let Some(inner) = node.child_by_field_name(spec.type_field) else {
        return InlineType::None;
    };
    // Only a DEFINITION is emitted, never a reference. `struct_specifier` is
    // the same node kind for both `struct Point { int x; }` (a definition,
    // which has a `body`) and the bare `struct Point` naming an existing type
    // in `typedef struct Point PointT;` (no `body`).
    //
    // Without this guard the reference re-emitted `Point` as a second Struct
    // node with a one-line span, so a typedef of an existing struct silently
    // produced a duplicate type in the graph. Caught by the parity corpus,
    // which contains exactly that construct — and pinned below by
    // `c_typedef_of_an_existing_struct_emits_no_duplicate`.
    if spec
        .body_field
        .and_then(|f| inner.child_by_field_name(f))
        .is_none()
    {
        return InlineType::None;
    }
    let is_anonymous = node_field_text(ctx.source, inner, spec.name_field).is_empty();
    let alias = at.alias.filter(|a| is_anonymous && !a.is_empty());
    if kind_in(cf.struct_like_kinds, inner.kind()) {
        emit_struct_named(
            w,
            ctx,
            inner,
            Placement {
                scope: at.scope,
                alias,
            },
        );
    } else if kind_in(cf.enum_like_kinds, inner.kind()) {
        emit_enum(w, ctx, inner, at.scope);
    } else {
        return InlineType::None;
    }
    if alias.is_some() {
        InlineType::Anonymous
    } else {
        InlineType::Named
    }
}

/// Emits a typedef as a `Constant` (`typedef=true`) + `Defines`. The name is the
/// first identifier leaf of the whole `type_definition` (LIFO DFS lands on the
/// declared alias, which follows the aliased type in child order).
fn emit_typedef(w: CWalk, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let CWalk { spec, cf } = w;
    let name = first_identifier(cf.naming, ctx.source, node);
    if name.is_empty() {
        return;
    }
    // `typedef struct { … } T;` / `typedef struct Tag { … } T;` carry the type
    // DEFINITION in the outer node's `type` field, which the flat top-level scan
    // never reached — so its fields were invisible (issue #107).
    //
    // An ANONYMOUS specifier is emitted under the typedef's own name, because
    // the alias is the only name that type has. In that case the alias IS the
    // struct, so no separate `typedef` Constant is emitted: doing both would put
    // two nodes on the same qualified name.
    //
    // A NAMED specifier (`typedef struct Tag { … } T;`) keeps both — `Tag` the
    // struct and `T` the alias are genuinely two names.
    let at = Placement {
        scope,
        alias: Some(&name),
    };
    if emit_inline_type(w, ctx, node, at) == InlineType::Anonymous {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CONSTANT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: vec![("typedef".to_string(), "true".to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits a function definition (`Function` + `Defines`, `{scope}::{name}#{seq}`)
/// and scans its `body_field` for calls via the shared generic call walker. The
/// name is the identifier the `declarator_field` chain binds — NOT a parameter
/// name (issue #106).
fn emit_function(w: CWalk, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let CWalk { spec, cf } = w;
    let name = node
        .child_by_field_name(cf.naming.declarator_field)
        .map(|d| declarator_name(cf.naming, ctx.source, d))
        .unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(scope, &name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FUNCTION.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: super::super::c_family::linkage_props(ctx.source, node),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a function prototype (`Function` with `is_prototype=true` + `Defines`,
/// `{scope}::{name}#{seq}`). No body ⇒ no calls. The name is resolved through
/// the declarator chain, skipping the parameter list, exactly as for a
/// definition — so `int add(int a, int b);` is `add`, not `b` (issue #106).
fn emit_prototype(w: CWalk, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let CWalk { spec, cf } = w;
    let name = node
        .child_by_field_name(cf.naming.declarator_field)
        .map(|d| declarator_name(cf.naming, ctx.source, d))
        .unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(scope, &name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FUNCTION.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: super::super::c_family::prototype_props(ctx.source, node),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}
