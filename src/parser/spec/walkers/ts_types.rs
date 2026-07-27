// parser::spec::walkers::ts_types — the TypeScript type-declaration emitters:
// classes, interfaces, enums, type aliases, their heritage edges, and their
// members (methods, fields, signatures, variants).
//
// Split out of `walkers/ts.rs` along that concern boundary (top-level dispatch +
// functions + value declarations there; type declarations and their bodies here)
// to keep both files inside the §4.1 500-line cap. The emitted
// `ExtractedNode`/`ExtractedRef` records are unchanged — this is the same
// hand-written-walker parity the `ts_parity_tests` corpus pins.

use tree_sitter::Node;

use super::super::families::TsFamilySpec;
use super::super::lang_spec::LangSpec;
use super::ts::{body_of, decl_visibility, member_visibility};
use super::{calls, end_line_of, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_ENUM, LABEL_FIELD,
    LABEL_METHOD, LABEL_STRUCT, LABEL_TRAIT, LABEL_TYPE_ALIAS, LABEL_VARIANT,
};

/// Emits a class as a `Struct` + `Defines`, its heritage edges
/// (`Extends`/`Implements`), then its body's methods and fields. The class node
/// carries NO `bases` property — the hand-written walker recorded inheritance
/// only as edges.
pub(super) fn emit_class(
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
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: decl_visibility(spec, &name, exported),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    emit_class_heritage(spec, tf, ctx, node, &qn);
    if let Some(body) = body_of(spec, node) {
        if kind_in(tf.class_body_kinds, body.kind()) {
            walk_class_body(spec, tf, ctx, body, &qn);
        }
    }
}

/// Emits one edge per supertype named in the class's heritage container:
/// `Extends` for an extends clause, `Implements` for an implements clause.
fn emit_class_heritage(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    class_qn: &str,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !kind_in(tf.class_heritage_kinds, child.kind()) {
            continue;
        }
        let mut inner = child.walk();
        for clause in child.children(&mut inner) {
            let k = clause.kind();
            if kind_in(tf.extends_clause_kinds, k) {
                emit_heritage_clause(spec, tf, ctx, clause, class_qn, "Extends", true);
            } else if kind_in(tf.implements_clause_kinds, k) {
                emit_heritage_clause(spec, tf, ctx, clause, class_qn, "Implements", true);
            }
        }
    }
}

/// Emits one `edge_kind` ref per supertype in one heritage clause. A bare
/// `heritage_name_kinds` child is taken verbatim; a generic application
/// (`Container<T>`) contributes its `name_field` text, but ONLY when
/// `with_generics` — the interface path passes `false` because the hand-written
/// walker dropped `interface I extends Comparable<string>` (parity, not intent).
fn emit_heritage_clause(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    clause: Node,
    owner_qn: &str,
    edge_kind: &'static str,
    with_generics: bool,
) {
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        let k = child.kind();
        let name = if kind_in(tf.heritage_name_kinds, k) {
            node_text(ctx.source, child)
        } else if with_generics && kind_in(tf.generic_type_kinds, k) {
            node_field_text(ctx.source, child, spec.name_field)
        } else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        ctx.refs.push(ExtractedRef {
            kind: edge_kind.to_string(),
            from_qualified_name: owner_qn.to_string(),
            to_qualified_name: name,
        });
    }
}

/// Walks a class body: `method_kinds` → `Method` + `HasMethod` (body scanned for
/// calls), `field_kinds` → `Field` + `HasField`. Every other member kind
/// (constructor overload signatures, `abstract_method_signature`, index
/// signatures, static blocks) is ignored, as it was before the migration.
fn walk_class_body(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    body: Node,
    class_qn: &str,
) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        let k = child.kind();
        if kind_in(tf.method_kinds, k) {
            emit_method(spec, tf, ctx, child, class_qn);
        } else if kind_in(tf.field_kinds, k) {
            emit_member_field(spec, tf, ctx, child, class_qn);
        }
    }
}

/// Emits a class method: `Method` (`is_async` from the conventions, then
/// `receiver_type`) + `HasMethod`, body scanned for calls. A getter and a setter
/// of the same name legitimately share one QN — the QN is NOT deduplicated.
fn emit_method(spec: &LangSpec, tf: &TsFamilySpec, ctx: &mut WalkCtx, node: Node, class_qn: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = spec.conventions.def_qn(class_qn, &name, 0);
    let mut props = spec.conventions.function_props(ctx.source, node);
    props.push(("receiver_type".to_string(), class_qn.to_string()));
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: member_visibility(tf, ctx.source, node),
        properties: props,
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: class_qn.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = body_of(spec, node) {
        calls::walk_calls(spec, ctx, body, &qn);
    }
}

/// Emits a class data member: `Field` (`type_annotation`, always present even
/// when empty) + `HasField`, visibility from its accessibility modifier.
fn emit_member_field(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    class_qn: &str,
) {
    emit_field(
        spec,
        ctx,
        node,
        class_qn,
        member_visibility(tf, ctx.source, node),
    );
}

/// The shared `Field` + `HasField` emitter for a class member and an interface
/// property (the two differ only in where visibility comes from).
fn emit_field(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, owner_qn: &str, visibility: String) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let fqn = qual(owner_qn, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FIELD.to_string(),
        name,
        qualified_name: fqn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility,
        properties: vec![(
            "type_annotation".to_string(),
            node_field_text(ctx.source, node, spec.type_field),
        )],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasField".to_string(),
        from_qualified_name: owner_qn.to_string(),
        to_qualified_name: fqn,
    });
}

/// Emits an interface as a `Trait` + `Defines`, its `extends` edges (bare names
/// only — see `emit_heritage_clause`), then its body's method and property
/// requirements.
pub(super) fn emit_interface(
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
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_TRAIT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: decl_visibility(spec, &name, exported),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    // The interface's extends clause is a DIRECT child (there is no heritage
    // container), and generic applications are deliberately not followed.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kind_in(tf.interface_extends_kinds, child.kind()) {
            emit_heritage_clause(spec, tf, ctx, child, &qn, "Extends", false);
        }
    }
    if let Some(body) = body_of(spec, node) {
        if kind_in(tf.interface_body_kinds, body.kind()) {
            walk_interface_body(spec, tf, ctx, body, &qn);
        }
    }
}

/// Walks an interface body: `method_signature_kinds` → `Method` + `HasMethod`,
/// `property_signature_kinds` → `Field` + `HasField`. Both carry empty
/// visibility (an interface member has no accessibility modifier).
fn walk_interface_body(
    spec: &LangSpec,
    tf: &TsFamilySpec,
    ctx: &mut WalkCtx,
    body: Node,
    iface_qn: &str,
) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        let k = child.kind();
        if kind_in(tf.method_signature_kinds, k) {
            emit_method_signature(spec, ctx, child, iface_qn);
        } else if kind_in(tf.property_signature_kinds, k) {
            emit_field(spec, ctx, child, iface_qn, String::new());
        }
    }
}

/// Emits an interface method requirement: `Method` + `HasMethod`, no body ⇒ no
/// calls. `is_async` is hardcoded `false` rather than sniffed from the node text:
/// a signature HAS no body and cannot be async, and the hand-written walker
/// hardcoded it too.
fn emit_method_signature(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, iface_qn: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = spec.conventions.def_qn(iface_qn, &name, 0);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: String::new(),
        properties: vec![
            ("is_async".to_string(), "false".to_string()),
            ("receiver_type".to_string(), iface_qn.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: iface_qn.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits an enum as an `Enum` + `Defines`, then one `Variant` + `HasVariant` per
/// member: an `enum_member_kinds` node is named by `name_field`
/// (`Red = "RED"`), an `enum_bare_member_kinds` node by its own text (`A`).
/// Variants carry no properties and empty visibility.
pub(super) fn emit_enum(
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
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_ENUM.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: decl_visibility(spec, &name, exported),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    // mutation note (§12): the `kind_in(tf.enum_body_kinds, …)` guard → `true`
    // mutant SURVIVES and is a proven EQUIVALENT. tree-sitter-typescript 0.23.2
    // constrains `enum_declaration.body` to exactly one type (`enum_body`), so
    // the guard can never reject a present body; and were it relaxed, iterating a
    // non-enum body would match none of `enum_member_kinds`/
    // `enum_bare_member_kinds` and emit nothing anyway. The same argument holds
    // for the `class_body_kinds` / `interface_body_kinds` guards below and in
    // `emit_class` — each grammar field admits a single body type. The guards are
    // kept as faithful copies of the hand-written walker's `body.kind() != …`
    // checks and as the seam a future ECMAScript dialect with a second body kind
    // would need. Not a coverage gap.
    // source: tree-sitter-typescript 0.23.2 node-types.json —
    // enum_declaration.body: [enum_body]; class_declaration.body /
    // abstract_class_declaration.body: [class_body];
    // interface_declaration.body: [interface_body].
    let body = match body_of(spec, node) {
        Some(b) if kind_in(tf.enum_body_kinds, b.kind()) => b,
        _ => return,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        let k = child.kind();
        let member = if kind_in(tf.enum_member_kinds, k) {
            node_field_text(ctx.source, child, spec.name_field)
        } else if kind_in(tf.enum_bare_member_kinds, k) {
            node_text(ctx.source, child)
        } else {
            continue;
        };
        if member.is_empty() {
            continue;
        }
        let vqn = qual(&qn, &member);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_VARIANT.to_string(),
            name: member,
            qualified_name: vqn.clone(),
            start_line: line_of(child),
            end_line: end_line_of(child),
            visibility: String::new(),
            properties: Vec::new(),
        });
        ctx.refs.push(ExtractedRef {
            kind: "HasVariant".to_string(),
            from_qualified_name: qn.clone(),
            to_qualified_name: vqn,
        });
    }
}

/// Emits a type alias as a `TypeAlias` (`target_type` = the aliased type's text,
/// empty when absent) + `Defines`.
pub(super) fn emit_type_alias(
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
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_TYPE_ALIAS.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: decl_visibility(spec, &name, exported),
        properties: vec![(
            "target_type".to_string(),
            node_field_text(ctx.source, node, tf.value_field),
        )],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}
