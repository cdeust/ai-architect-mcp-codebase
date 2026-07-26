// parser::spec::walkers::types — the type-declaration walkers (Go-style
// `type` decls: struct/interface/alias, their fields, and base-class names).
//
// Split out of `walkers/mod.rs` (issue #101, §4.1) as a pure move; the
// `ExtractedNode`/`ExtractedRef` contract is unchanged.

use tree_sitter::Node;

use super::super::lang_spec::LangSpec;
use super::{end_line_of, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_FIELD, LABEL_STRUCT,
    LABEL_TRAIT, LABEL_TYPE_ALIAS,
};

/// Base-class names: the `base_node_kinds` children of the class's
/// `extends_field`, verbatim (attribute access like `typing.NamedTuple` is
/// preserved; the resolver looks up by the last segment). Empty when the
/// language has no `extends_field`. Called by the default
/// `LanguageConventions::class_inheritance` (Python); Java overrides that
/// method and does not use this.
pub(in crate::parser::spec) fn collect_bases(
    spec: &LangSpec,
    source: &str,
    class_node: Node,
) -> Vec<String> {
    let field = match spec.extends_field {
        Some(f) => f,
        None => return Vec::new(),
    };
    let superclasses = match class_node.child_by_field_name(field) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut names = Vec::new();
    let mut cursor = superclasses.walk();
    for child in superclasses.children(&mut cursor) {
        if kind_in(spec.base_node_kinds, child.kind()) {
            let text = node_text(source, child);
            if !text.is_empty() {
                names.push(text);
            }
        }
    }
    names
}

pub(super) fn walk_type_decl(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kind_in(spec.type_spec_node_kinds, child.kind()) {
            emit_type_spec(spec, ctx, child, scope);
        }
    }
}

fn emit_type_spec(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let mut label = LABEL_TYPE_ALIAS;
    let mut struct_type: Option<Node> = None;
    if let Some(ty) = node.child_by_field_name(spec.type_field) {
        if spec.struct_type_kind == Some(ty.kind()) {
            label = LABEL_STRUCT;
            struct_type = Some(ty);
        } else if spec.interface_type_kind == Some(ty.kind()) {
            label = LABEL_TRAIT;
        }
    }
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
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
    if let Some(st) = struct_type {
        walk_fields(spec, ctx, st, &qn);
    }
}

/// Emits one `Field` node + `HasField` edge per declared name inside a struct.
/// A field declaration may declare several names (`X, Y int`), so every child
/// carrying the `name` field is emitted — matching the grammar's `multiple`
/// name field.
fn walk_fields(spec: &LangSpec, ctx: &mut WalkCtx, container_parent: Node, owner_qn: &str) {
    let mut c1 = container_parent.walk();
    for container in container_parent.children(&mut c1) {
        if !kind_in(spec.field_container_kinds, container.kind()) {
            continue;
        }
        let mut c2 = container.walk();
        for fd in container.children(&mut c2) {
            if !kind_in(spec.field_node_kinds, fd.kind()) {
                continue;
            }
            let type_text = node_field_text(ctx.source, fd, spec.type_field);
            let mut names: Vec<Node> = Vec::new();
            let mut c3 = fd.walk();
            if c3.goto_first_child() {
                loop {
                    if c3.field_name() == Some(spec.name_field) {
                        names.push(c3.node());
                    }
                    if !c3.goto_next_sibling() {
                        break;
                    }
                }
            }
            for name_node in names {
                let fname = node_text(ctx.source, name_node);
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
}
