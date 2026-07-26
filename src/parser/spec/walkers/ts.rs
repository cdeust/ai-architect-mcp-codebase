// parser::spec::walkers::ts — the ECMAScript-family definition walker
// (ADR-0055 phase 7, TypeScript). Reproduces the pre-migration hand-written
// TypeScript walker (`parser::typescript::extract`) at exact parity, driven by
// the `TsFamilySpec` sub-table instead of hardcoded `TS_*` constants.
//
// Why a dedicated walker rather than the class-model `walk_defs`: see
// `TsFamilySpec`'s type doc — export is a WRAPPER carrying visibility for its
// inner declaration, a `const` binding becomes a `Function` or a `Constant`
// depending on its value, class AND interface bodies emit both methods and
// fields from four member kinds, and def QNs are deliberately NOT deduplicated
// (a getter/setter pair shares one QN). `walk_defs` delegates here whenever a
// `LangSpec` carries `ts_family: Some(_)`; the seven languages riding
// `walk_defs`/`clike`/`cpp` are untouched. Calls and imports still route through
// the SHARED generic walkers (`calls::walk_calls`, `imports::walk_imports`) via
// the conventions — only the definition shapes are TypeScript-specific.
//
// `exported` is threaded down from the `export_statement` wrapper. It is the
// ONLY signal for a `variable_declarator` (`export const f = () => {}`), while a
// function/class/interface/enum/type-alias ALSO counts as exported when its
// previous sibling is the `export` token. That asymmetry is the hand-written
// walker's (`extract_variable_declarator` never called `has_export_keyword`),
// and parity preserves it.

use tree_sitter::Node;

use super::super::families::TsFamilySpec;
use super::super::lang_spec::LangSpec;
use super::ts_types::{emit_class, emit_enum, emit_interface, emit_type_alias};
use super::{calls, end_line_of, imports, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_FUNCTION,
};

/// Top-level walker: every direct child of the file root is a declaration, an
/// import statement, or an export wrapper. Nothing else is descended — the
/// hand-written walker ran `extract_top_level` exactly once, at the root, so a
/// function nested inside another function's body is NOT a definition (only its
/// calls are collected, by the call walker).
pub(super) fn walk_ts_defs(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    parent: Node,
    scope: &str,
) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        let k = child.kind();
        if emit_declaration(spec, tf, ctx, child, scope, /*exported=*/ false) {
            continue;
        }
        if kind_in(spec.import_node_kinds, k) {
            imports::walk_imports(spec, ctx, child, scope);
        } else if kind_in(tf.export_stmt_kinds, k) {
            // `export … ;` wraps a declaration; walk its children as exported.
            // Import statements are NOT dispatched here (an `export` wrapper
            // never holds one, and the hand-written `extract_export` matched
            // only the six declaration kinds).
            let mut inner = child.walk();
            for decl in child.children(&mut inner) {
                emit_declaration(spec, tf, ctx, decl, scope, /*exported=*/ true);
            }
        }
    }
}

/// Dispatches one declaration node to its emitter. Returns whether the node was
/// a declaration this walker owns, so the caller can fall through to the
/// import/export arms. The six slices are disjoint, so arm order is immaterial.
fn emit_declaration(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    exported: bool,
) -> bool {
    let k = node.kind();
    if kind_in(tf.func_decl_kinds, k) {
        emit_function(spec, tf, ctx, node, scope, exported);
    } else if kind_in(tf.class_decl_kinds, k) {
        emit_class(spec, tf, ctx, node, scope, exported);
    } else if kind_in(tf.interface_decl_kinds, k) {
        emit_interface(spec, tf, ctx, node, scope, exported);
    } else if kind_in(tf.enum_decl_kinds, k) {
        emit_enum(spec, tf, ctx, node, scope, exported);
    } else if kind_in(tf.type_alias_kinds, k) {
        emit_type_alias(spec, tf, ctx, node, scope, exported);
    } else if kind_in(tf.value_decl_kinds, k) {
        walk_value_decl(spec, tf, ctx, node, scope, exported);
    } else {
        return false;
    }
    true
}

/// The visibility of a top-level declaration: `pub` when the enclosing wrapper
/// marked it exported OR its previous sibling is the `export` token, else the
/// conventions' name-based rule (empty for TypeScript). The sibling check is
/// what makes `export function f(){}` public without the wrapper flag, and what
/// `export default function f(){}` (whose previous sibling is `default`) needs
/// the wrapper flag for.
pub(super) fn decl_visibility(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    node: Node,
    name: &str,
    exported: bool,
) -> String {
    if exported || has_export_sibling(tf, node) {
        return "pub".to_string();
    }
    spec.conventions.visibility_of(name)
}

fn has_export_sibling(tf: &TsFamilySpec, node: Node) -> bool {
    node.prev_sibling()
        .is_some_and(|prev| prev.kind() == tf.export_keyword_kind)
}

/// The visibility of a class/interface member: the first `visibility_modifier_kinds`
/// direct child's text (`public`/`private`/`protected`), else empty. Interface
/// members never carry one, so they are uniformly empty.
pub(super) fn member_visibility(tf: &TsFamilySpec, source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| kind_in(tf.visibility_modifier_kinds, c.kind()))
        .map(|c| node_text(source, c));
    found.unwrap_or_default()
}

/// The node's `body_field` child, or `None`. TypeScript names every body
/// (function, class, interface, enum, arrow) through the same field, so a
/// missing one means "no body" (an overload signature, an abstract method).
pub(super) fn body_of<'t>(spec: &LangSpec, node: Node<'t>) -> Option<Node<'t>> {
    spec.body_field.and_then(|f| node.child_by_field_name(f))
}

/// Emits a `Function` + `Defines`, then scans its body for calls. The QN comes
/// from the conventions' `def_qn` (TypeScript: `{scope}::{name}`, NOT
/// deduplicated) and `is_async` from `function_props`.
fn emit_function(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    exported: bool,
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    // `seq` is unused by the TypeScript `def_qn` (its call sites key on the byte
    // span instead), so the shared per-file counter is deliberately not consumed.
    let qn = spec.conventions.def_qn(scope, &name, 0);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FUNCTION.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: decl_visibility(spec, tf, node, &name, exported),
        properties: spec.conventions.function_props(ctx.source, node),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = body_of(spec, node) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}

/// Walks a `const`/`let`/`var` statement: each DIRECT declarator child becomes a
/// `Function` (arrow value) or a `Constant` (`const` statement). Only direct
/// children are visited — a declarator nested inside an arrow's body is not a
/// top-level definition.
fn walk_value_decl(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    exported: bool,
) {
    let is_const = spec.conventions.is_const_decl(ctx.source, node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kind_in(tf.declarator_kinds, child.kind()) {
            emit_declarator(spec, tf, ctx, child, scope, is_const, exported);
        }
    }
}

/// Emits one declarator. An arrow value makes it a `Function` + `Defines` whose
/// LINE SPAN is the declarator's (not the arrow's), whose `is_async` comes from
/// the ARROW node, and whose call scan is the arrow's body. Otherwise a `const`
/// statement makes it a `Constant` + `Defines` (`type_annotation` from the
/// declarator); a `let`/`var` non-arrow binding emits nothing.
///
/// Visibility here consults ONLY the inherited `exported` flag — never the
/// `export` sibling token — because the hand-written walker did not (see this
/// module's header).
fn emit_declarator(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    is_const: bool,
    exported: bool,
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let visibility = if exported {
        "pub".to_string()
    } else {
        spec.conventions.visibility_of(&name)
    };
    let arrow = node
        .child_by_field_name(tf.value_field)
        .filter(|v| kind_in(tf.arrow_func_kinds, v.kind()));
    if let Some(arrow) = arrow {
        let qn = spec.conventions.def_qn(scope, &name, 0);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_FUNCTION.to_string(),
            name: name.clone(),
            qualified_name: qn.clone(),
            start_line: line_of(node),
            end_line: end_line_of(node),
            visibility,
            properties: spec.conventions.function_props(ctx.source, arrow),
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn.clone(),
        });
        if let Some(body) = body_of(spec, arrow) {
            calls::walk_calls(spec, ctx, body, &qn);
        }
        return;
    }
    if !is_const {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CONSTANT.to_string(),
        name,
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility,
        properties: vec![(
            "type_annotation".to_string(),
            node_field_text(ctx.source, node, spec.type_field),
        )],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}
