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
use super::clike_types::{emit_enum, emit_inline_type, emit_struct, emit_typedef, Placement};
use super::declarator::{binds_function_prototype, declarator_name};
use super::{calls, end_line_of, imports, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_FUNCTION,
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
pub(super) struct CWalk<'s> {
    pub(super) spec: &'s LangSpec,
    pub(super) cf: &'s CFamilySpec,
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
