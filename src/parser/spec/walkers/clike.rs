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
// This walker is the shared abstraction for the whole C family: C migrates onto
// it now; C++ and ObjC land as two more `CFamilySpec` rows, collapsing the
// duplicated `extract_function`/`extract_struct` walkers ADR-0055 names.

use tree_sitter::Node;

use super::super::lang_spec::{CFamilySpec, LangSpec};
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
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if kind_in(cf.struct_like_kinds, k) {
            emit_struct(spec, cf, ctx, child, scope);
        } else if kind_in(cf.enum_like_kinds, k) {
            emit_enum(spec, cf, ctx, child, scope);
        } else if kind_in(cf.typedef_kinds, k) {
            emit_typedef(spec, cf, ctx, child, scope);
        } else if kind_in(cf.func_def_kinds, k) {
            emit_function(spec, cf, ctx, child, scope);
        } else if kind_in(spec.import_node_kinds, k) {
            imports::walk_imports(spec, ctx, child, scope);
        } else if kind_in(cf.func_decl_kinds, k) {
            // `declaration` is shared by prototypes (`int f(void);`) and plain
            // variable declarations (`int x;`); only the former — carrying a
            // function declarator — is emitted, matching the hand-written walker.
            if is_c_function_prototype(cf, child) {
                emit_prototype(spec, cf, ctx, child, scope);
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
            walk_c_defs(spec, cf, ctx, child, scope);
        }
    }
}

/// The first identifier leaf (`identifier_kinds`) found in a right-to-left DFS of
/// `node`. This reproduces the hand-written `find_identifier`'s stack-DFS order
/// EXACTLY (children pushed left-to-right onto a LIFO stack, so the last child's
/// subtree is searched first). That order is load-bearing: for a function
/// declarator `add(int a, int b)` it returns the LAST parameter name (`b`), not
/// the function name — a pre-existing defect the migration preserves for parity
/// (filed as issue #106). Not "the name" — "the name the old walker produced".
fn find_identifier(cf: &CFamilySpec, source: &str, node: Node) -> String {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if kind_in(cf.identifier_kinds, n.kind()) {
            return node_text(source, n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    String::new()
}

/// The first `field_identifier` leaf found in a right-to-left DFS of `node`,
/// unwrapping pointer/array/function declarators to the bare field name
/// (`int *p` → `p`, `char buf[8]` → `buf`, `int (*h)(int)` → `h`). Same LIFO-DFS
/// order as `find_identifier` — reproduces the hand-written `find_field_identifier`.
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

/// A `func_decl_kinds` node is a function prototype iff it carries a function
/// declarator directly, or inside an `init_declarator` (`int f(void) = …`).
/// A plain variable declaration (`int x;`) carries neither and is not a prototype.
fn is_c_function_prototype(cf: &CFamilySpec, node: Node) -> bool {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == cf.func_declarator_kind {
            return true;
        }
        if c.kind() == cf.init_declarator_kind {
            let mut ic = c.walk();
            for gc in c.children(&mut ic) {
                if gc.kind() == cf.func_declarator_kind {
                    return true;
                }
            }
        }
    }
    false
}

/// The declaration's name: the `name` field's text, or — when absent (an
/// anonymous struct/union/enum) — the first identifier leaf (LIFO DFS). Matches
/// the hand-written `node_field_text(node,"name")`-then-`find_identifier` fallback.
fn named_or_first_identifier(
    cf: &CFamilySpec,
    spec: &LangSpec,
    source: &str,
    node: Node,
) -> String {
    let field = node_field_text(source, node, spec.name_field);
    if field.is_empty() {
        find_identifier(cf, source, node)
    } else {
        field
    }
}

/// Emits a struct/union (`Struct` + `Defines`) and, from its `body_field`, one
/// `Field` + `HasField` per declared member (declarators unwrapped to their
/// field name; a member with no field name — an anonymous member — is skipped).
fn emit_struct(spec: &LangSpec, cf: &CFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = named_or_first_identifier(cf, spec, ctx.source, node);
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
        emit_struct_fields(spec, cf, ctx, body, &qn);
    }
}

/// Emits one `Field` + `HasField` per declared name in each `field_decl_kinds`
/// member of `body`. A single `field_declaration` may declare several names
/// Every `declarator_field` child of `fd`, in source order. A single member
/// declaration binds several names through repeated `declarator` fields
/// (`int a, b, c;`), so `child_by_field_name` (first-only) is insufficient — the
/// cursor walk collects them all, matching the hand-written multi-declarator loop.
fn declarator_field_children<'t>(cf: &CFamilySpec, fd: Node<'t>) -> Vec<Node<'t>> {
    let mut declarators: Vec<Node> = Vec::new();
    let mut dc = fd.walk();
    if dc.goto_first_child() {
        loop {
            if dc.field_name() == Some(cf.declarator_field) {
                declarators.push(dc.node());
            }
            if !dc.goto_next_sibling() {
                break;
            }
        }
    }
    declarators
}

/// (`int a, b, c;`), so every `declarator_field` child is emitted; each is
/// unwrapped (pointer/array/function) to its `field_identifier`. The type
/// annotation is the shared `type_field` text.
fn emit_struct_fields(
    spec: &LangSpec,
    cf: &CFamilySpec,
    ctx: &mut WalkCtx,
    body: Node,
    owner_qn: &str,
) {
    let mut bc = body.walk();
    for fd in body.children(&mut bc) {
        if !kind_in(cf.field_decl_kinds, fd.kind()) {
            continue;
        }
        let type_text = node_field_text(ctx.source, fd, spec.type_field);
        for declarator in declarator_field_children(cf, fd) {
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
fn emit_enum(spec: &LangSpec, cf: &CFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = named_or_first_identifier(cf, spec, ctx.source, node);
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
        let en = find_identifier(cf, ctx.source, child);
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

/// Emits a typedef as a `Constant` (`typedef=true`) + `Defines`. The name is the
/// first identifier leaf of the whole `type_definition` (LIFO DFS lands on the
/// declared alias, which follows the aliased type in child order).
fn emit_typedef(spec: &LangSpec, cf: &CFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = find_identifier(cf, ctx.source, node);
    if name.is_empty() {
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
/// name is the first identifier leaf of the `declarator_field` child.
fn emit_function(spec: &LangSpec, cf: &CFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node
        .child_by_field_name(cf.declarator_field)
        .map(|d| find_identifier(cf, ctx.source, d))
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
        properties: Vec::new(),
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
/// `{scope}::{name}#{seq}`). No body ⇒ no calls. The name is the first
/// identifier leaf of the WHOLE declaration node (matching the hand-written
/// `find_identifier(node)`, which — like the definition — lands on the last
/// parameter name when parameters are named).
fn emit_prototype(spec: &LangSpec, cf: &CFamilySpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = find_identifier(cf, ctx.source, node);
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
        properties: vec![("is_prototype".to_string(), "true".to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}
