// parser::spec::walkers::objc::c_constructs — the C-superset half of the
// Objective-C walker (ADR-0055 phase 8). Split from `objc/mod.rs` along the
// concern boundary (§4.1): this file emits the plain-C declarations a `.m`/`.h`
// file carries (structs/unions with fields, enums with members, typedefs),
// while `objc/mod.rs` handles the ObjC object model (classes, protocols,
// methods, functions). Both consume the same `ObjcFamilySpec` sub-table and the
// shared `WalkCtx`, at EXACT parity with the hand-written walker.
//
// The C-side name resolution deliberately differs from `walkers::clike`'s: the
// hand-written ObjC walker named C structs/enums by the `name` field then the
// first BARE `identifier` (NOT the parameter-skipping declarator chain), and
// named typedefs by the LAST `type_identifier` under the declarator, and it did
// NOT recurse a typedef's inline struct definition (issue #127). Those
// differences are preserved here for parity.

use tree_sitter::Node;

use super::super::super::lang_spec::{LangSpec, ObjcFamilySpec};
use super::super::{end_line_of, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_ENUM,
    LABEL_FIELD, LABEL_STRUCT,
};

/// The text of the first direct child of exactly `plain_identifier_kind`
/// (`identifier`), or empty. Matches the hand-written `first_identifier` — and
/// is deliberately narrower than `objc/mod.rs`'s `find_name` (see
/// `plain_identifier_kind`): an anonymous C type has no bare `identifier` child,
/// so it resolves to empty and is skipped.
fn first_identifier(of: &ObjcFamilySpec, source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    let text = node
        .children(&mut cursor)
        .find(|c| c.kind() == of.plain_identifier_kind)
        .map(|c| node_text(source, c))
        .unwrap_or_default();
    text
}

/// The name of a C struct/union/enum: the `name_field` text, or the first bare
/// `identifier`. Matches the hand-written `node_field_text(node,"name")`-then-
/// `first_identifier` fallback (an anonymous C type resolves to empty here).
fn c_type_name(spec: &LangSpec, of: &ObjcFamilySpec, source: &str, node: Node) -> String {
    let n = node_field_text(source, node, spec.name_field);
    if n.is_empty() {
        first_identifier(of, source, node)
    } else {
        n
    }
}

/// Emits a C `struct`/`union` as a `Struct` + `Defines` and, from its
/// `body_field`, one `Field` + `HasField` per declared member. The name is the
/// `name_field` text then the first bare `identifier` (anonymous ⇒ skipped).
/// Matches the hand-written `extract_c_struct`.
pub(super) fn emit_c_struct(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    let name = c_type_name(spec, of, ctx.source, node);
    if name.is_empty() {
        return; // anonymous struct (e.g. inside a typedef) — skip
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
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
        emit_c_struct_fields(spec, of, ctx, body, &qn);
    }
}

/// Emits one `Field` + `HasField` per declared name in each `field_decl_kinds`
/// member of `body`. A single member declaration binds several names through
/// repeated `declarator` fields (`int a, b;`), each unwrapped to its
/// `field_identifier`. Matches the hand-written `extract_c_struct_fields`.
fn emit_c_struct_fields(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    body: Node,
    owner_qn: &str,
) {
    let mut cursor = body.walk();
    for fd in body.children(&mut cursor) {
        if !kind_in(of.field_decl_kinds, fd.kind()) {
            continue;
        }
        let type_text = node_field_text(ctx.source, fd, spec.type_field);
        let mut declarators: Vec<Node> = Vec::new();
        let mut dc = fd.walk();
        if dc.goto_first_child() {
            loop {
                if dc.field_name() == Some(of.declarator_field) {
                    declarators.push(dc.node());
                }
                if !dc.goto_next_sibling() {
                    break;
                }
            }
        }
        for declarator in declarators {
            let fname = find_field_identifier(of, ctx.source, declarator);
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

/// The first `field_identifier_kind` leaf under a (possibly pointer/array)
/// declarator, in a LIFO DFS. Matches the hand-written `find_c_field_identifier`.
fn find_field_identifier(of: &ObjcFamilySpec, source: &str, node: Node) -> String {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == of.field_identifier_kind {
            return node_text(source, n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    String::new()
}

/// Emits a C `enum` as an `Enum` + `Defines` and its `enum_member_kinds` entries
/// as `Constant`s (`enum_entry=true`) + `Defines` scoped under the enum. Matches
/// the hand-written `extract_c_enum`.
pub(super) fn emit_c_enum(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    let name = c_type_name(spec, of, ctx.source, node);
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
        if !kind_in(of.enum_member_kinds, child.kind()) {
            continue;
        }
        let en = c_type_name(spec, of, ctx.source, child);
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

/// Emits a C `typedef` as a `Constant` (`typedef=true`) + `Defines`. The name is
/// the LAST `typedef_name_kind` (`type_identifier`) reached through the
/// `declarator_field` (unwrapping pointer/array declarators), falling back to a
/// full-node DFS. The inline struct definition of a `typedef struct { … } T;` is
/// deliberately NOT recursed — its fields are dropped, a pre-existing defect
/// preserved for parity (issue #127). Matches `extract_c_typedef` +
/// `find_c_typedef_name`.
pub(super) fn emit_c_typedef(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    let name = find_typedef_name(of, ctx.source, node);
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

/// The declared alias of a typedef: the LAST `typedef_name_kind` reached in a
/// LIFO DFS starting at the `declarator_field` child (or the whole node when the
/// field is absent). Matches the hand-written `find_c_typedef_name`.
fn find_typedef_name(of: &ObjcFamilySpec, source: &str, node: Node) -> String {
    let start = node
        .child_by_field_name(of.declarator_field)
        .unwrap_or(node);
    let mut last = String::new();
    let mut stack = vec![start];
    while let Some(n) = stack.pop() {
        if n.kind() == of.typedef_name_kind {
            last = node_text(source, n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    last
}
