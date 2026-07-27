// parser::spec::walkers::typescript::members — the interface / enum /
// type-alias / value-declaration emitters for the TypeScript walker
// (ADR-0055 phase 9). Split from `walkers/typescript/mod.rs` along the §4.1
// concern boundary; the `ExtractedNode`/`ExtractedRef` contract is unchanged.

use tree_sitter::Node;

use super::super::super::lang_spec::{LangSpec, TsFamilySpec};
use super::super::{end_line_of, kind_in, line_of, WalkCtx};
use super::{export_vis, has_async, ts_walk_calls};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_ENUM,
    LABEL_FIELD, LABEL_FUNCTION, LABEL_METHOD, LABEL_TRAIT, LABEL_TYPE_ALIAS, LABEL_VARIANT,
};

/// Emits an interface (`Trait` + `Defines`), one `Extends` per base in its
/// `extends_type_clause` (bare names only — no `generic_type` unwrapping, as in
/// the hand-written walker), then recurses its body for method/property
/// signatures.
pub(super) fn emit_interface(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    is_exported: bool,
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_TRAIT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: export_vis(is_exported),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    emit_interface_extends(tf, ctx, node, &qn);
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        emit_interface_body(spec, tf, ctx, body, &qn);
    }
}

/// One `Extends` edge per base named in an interface's `extends_type_clause`.
fn emit_interface_extends(tf: &TsFamilySpec, ctx: &mut WalkCtx, node: Node, iface_qn: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !kind_in(tf.interface_extends_kinds, child.kind()) {
            continue;
        }
        let mut hcursor = child.walk();
        for hchild in child.children(&mut hcursor) {
            if kind_in(tf.heritage_name_kinds, hchild.kind()) {
                let name = node_text(ctx.source, hchild);
                if !name.is_empty() {
                    ctx.refs.push(ExtractedRef {
                        kind: "Extends".to_string(),
                        from_qualified_name: iface_qn.to_string(),
                        to_qualified_name: name,
                    });
                }
            }
        }
    }
}

/// Recurses an interface body, emitting a `Method` (`is_async=false`) per
/// `method_signature` and a `Field` per `property_signature`. `interface_body`
/// and `object_type` both route here.
fn emit_interface_body(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    body: Node,
    iface_qn: &str,
) {
    if !kind_in(tf.interface_body_kinds, body.kind()) {
        return;
    }
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        let k = child.kind();
        if kind_in(tf.method_sig_kinds, k) {
            let name = node_field_text(ctx.source, child, spec.name_field);
            if name.is_empty() {
                continue;
            }
            let mqn = qual(iface_qn, &name);
            ctx.nodes.push(ExtractedNode {
                label: LABEL_METHOD.to_string(),
                name: name.clone(),
                qualified_name: mqn.clone(),
                start_line: line_of(child),
                end_line: end_line_of(child),
                visibility: String::new(),
                properties: vec![
                    ("is_async".to_string(), "false".to_string()),
                    ("receiver_type".to_string(), iface_qn.to_string()),
                ],
            });
            ctx.refs.push(ExtractedRef {
                kind: "HasMethod".to_string(),
                from_qualified_name: iface_qn.to_string(),
                to_qualified_name: mqn,
            });
        } else if kind_in(tf.property_sig_kinds, k) {
            let name = node_field_text(ctx.source, child, spec.name_field);
            if name.is_empty() {
                continue;
            }
            let type_ann = node_field_text(ctx.source, child, spec.type_field);
            let fqn = qual(iface_qn, &name);
            ctx.nodes.push(ExtractedNode {
                label: LABEL_FIELD.to_string(),
                name,
                qualified_name: fqn.clone(),
                start_line: line_of(child),
                end_line: end_line_of(child),
                visibility: String::new(),
                properties: vec![("type_annotation".to_string(), type_ann)],
            });
            ctx.refs.push(ExtractedRef {
                kind: "HasField".to_string(),
                from_qualified_name: iface_qn.to_string(),
                to_qualified_name: fqn,
            });
        }
    }
}

/// Emits an enum (`Enum` + `Defines`), then one `Variant` + `HasVariant` per
/// member: an `enum_assignment` (named by `name`) or a bare `property_identifier`
/// (named by its own text).
pub(super) fn emit_enum(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    is_exported: bool,
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
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
        visibility: export_vis(is_exported),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) {
        emit_enum_members(spec, tf, ctx, body, &qn);
    }
}

fn emit_enum_members(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    body: Node,
    enum_qn: &str,
) {
    if !kind_in(tf.enum_body_kinds, body.kind()) {
        return;
    }
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        let k = child.kind();
        let name = if kind_in(tf.enum_assignment_kinds, k) {
            node_field_text(ctx.source, child, spec.name_field)
        } else if kind_in(tf.enum_member_ident_kinds, k) {
            node_text(ctx.source, child)
        } else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let vqn = qual(enum_qn, &name);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_VARIANT.to_string(),
            name,
            qualified_name: vqn.clone(),
            start_line: line_of(child),
            end_line: end_line_of(child),
            visibility: String::new(),
            properties: Vec::new(),
        });
        ctx.refs.push(ExtractedRef {
            kind: "HasVariant".to_string(),
            from_qualified_name: enum_qn.to_string(),
            to_qualified_name: vqn,
        });
    }
}

/// Emits a `TypeAlias` + `Defines` with a `target_type` property (the `value`
/// field's text).
pub(super) fn emit_type_alias(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    is_exported: bool,
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let target = node
        .child_by_field_name(tf.value_field)
        .map(|n| node_text(ctx.source, n))
        .unwrap_or_default();
    ctx.nodes.push(ExtractedNode {
        label: LABEL_TYPE_ALIAS.to_string(),
        name,
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: export_vis(is_exported),
        properties: vec![("target_type".to_string(), target)],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Walks a `lexical_declaration`/`variable_declaration`, dispatching each
/// `variable_declarator`. `const`-ness is read from the declaration's leading
/// keyword (only `const` yields a `Constant`; a `let`/`var` non-arrow yields
/// nothing).
pub(super) fn emit_value_decl(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    is_exported: bool,
) {
    let is_const = ctx.source[node.byte_range()].starts_with("const ");
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kind_in(tf.declarator_kinds, child.kind()) {
            emit_declarator(spec, tf, ctx, child, scope, is_const, is_exported);
        }
    }
}

/// Emits one `variable_declarator`: an `arrow_function` value → a call-scanning
/// `Function`; a `const` non-arrow → a `Constant` (with `type_annotation`); a
/// `let`/`var` non-arrow → nothing. A non-arrow object-literal value emits only
/// the `Constant` and its methods' bodies are NOT scanned for calls — a
/// pre-existing defect preserved for parity (issue #142).
fn emit_declarator(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    is_const: bool,
    is_exported: bool,
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let value = node.child_by_field_name(tf.value_field);
    let arrow = value.filter(|v| kind_in(tf.arrow_kinds, v.kind()));

    if let Some(arrow) = arrow {
        let qn = spec.conventions.def_qn(scope, &name, 0);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_FUNCTION.to_string(),
            name: name.clone(),
            qualified_name: qn.clone(),
            start_line: line_of(node),
            end_line: end_line_of(node),
            visibility: export_vis(is_exported),
            properties: vec![(
                "is_async".to_string(),
                has_async(ctx.source, arrow).to_string(),
            )],
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn.clone(),
        });
        if let Some(body) = spec.body_field.and_then(|f| arrow.child_by_field_name(f)) {
            ts_walk_calls(spec, ctx, body, &qn);
        }
    } else if is_const {
        let qn = qual(scope, &name);
        let type_ann = node_field_text(ctx.source, node, spec.type_field);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_CONSTANT.to_string(),
            name,
            qualified_name: qn.clone(),
            start_line: line_of(node),
            end_line: end_line_of(node),
            visibility: export_vis(is_exported),
            properties: vec![("type_annotation".to_string(), type_ann)],
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn,
        });
    }
}
