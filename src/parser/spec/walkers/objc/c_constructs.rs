// parser::spec::walkers::objc::c_constructs — the C-superset half of the
// Objective-C walker (ADR-0055 phase 8). Split from `objc/mod.rs` along the
// concern boundary (§4.1): this file emits the plain-C declarations a `.m`/`.h`
// file carries (structs/unions with fields, enums with members, typedefs),
// while `objc/mod.rs` handles the ObjC object model (classes, protocols,
// methods, functions). Both consume the same `ObjcFamilySpec` sub-table and the
// shared `WalkCtx`, at EXACT parity with the hand-written walker.
//
// The C-side name resolution deliberately differs from `walkers::clike`'s: this
// lane names C structs/enums by the `name` field alone (NOT the
// parameter-skipping declarator chain) and typedefs by the LAST `type_identifier`
// under the declarator. Unlike the original hand-written walker, it DOES recurse
// a typedef's inline struct definition (issue #127). Those differences are ObjC's
// name resolution, not a defect.

use tree_sitter::Node;

use super::super::super::lang_spec::{LangSpec, ObjcFamilySpec};
use super::super::{end_line_of, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_ENUM,
    LABEL_FIELD, LABEL_STRUCT,
};

/// The name of a C struct/union/enum (or an enumerator): the `name_field` text,
/// empty when the specifier is anonymous.
///
/// Preconditions: `node` is a struct/union/enum specifier or an enumerator.
/// Postconditions: the declared name, or `""` for an anonymous specifier — which
/// every caller treats as "emit nothing".
///
/// This used to fall back to "the first bare `identifier` child" when the `name`
/// field was absent. That fallback was unreachable: in tree-sitter-objc 3.0.2 a
/// struct/union/enum specifier's only non-field children are `attribute_specifier`
/// and `ms_declspec_modifier`, so a bare `identifier` is never a direct child and
/// the fallback could only ever return `""`. Mutation testing proved it — every
/// mutant that made the helper return empty SURVIVED, which is the signature of
/// dead code, so it is removed rather than pinned (§9, §12.1). Absorbed from
/// fix/cpp-extraction-123-124 commit f7c4fb2.
/// source: tree-sitter-objc 3.0.2 node-types.json (struct_specifier /
/// union_specifier / enum_specifier: fields `name`/`body`, children
/// `attribute_specifier`/`ms_declspec_modifier`; enumerator: field `name`).
fn c_type_name(spec: &LangSpec, source: &str, node: Node) -> String {
    node_field_text(source, node, spec.name_field)
}

/// Emits a C `struct`/`union` as a `Struct` + `Defines` and, from its
/// `body_field`, one `Field` + `HasField` per declared member. The name is the
/// `name_field` text (anonymous ⇒ skipped). Matches the hand-written
/// `extract_c_struct`.
pub(super) fn emit_c_struct(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    emit_c_struct_named(spec, of, ctx, node, scope, None);
}

/// `emit_c_struct` with an optional name override for an ANONYMOUS specifier.
///
/// `typedef struct { int x; } T;` declares a type whose only usable name is the
/// typedef alias (issue #127). Without the override the specifier has no `name`
/// field and no bare `identifier` child, so it resolves to empty and its FIELDS
/// are dropped. Mirrors the C walker's `emit_struct_named` (#107).
fn emit_c_struct_named(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    override_name: Option<&str>,
) {
    let name = match override_name {
        Some(n) => n.to_string(),
        None => c_type_name(spec, ctx.source, node),
    };
    if name.is_empty() {
        return; // anonymous struct with no alias to borrow — skip
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
    emit_c_enum_named(spec, of, ctx, node, scope, None);
}

/// `emit_c_enum` with an optional name override for an ANONYMOUS specifier —
/// `typedef enum { A } E;`, whose only name is the alias (issue #127), the enum
/// analog of `emit_c_struct_named`.
fn emit_c_enum_named(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    override_name: Option<&str>,
) {
    let name = match override_name {
        Some(n) => n.to_string(),
        None => c_type_name(spec, ctx.source, node),
    };
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
        let en = c_type_name(spec, ctx.source, child);
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

/// What `emit_typedef_inline_type` found, so `emit_c_typedef` knows whether the
/// alias name has already been consumed by an anonymous type.
#[derive(PartialEq, Eq)]
enum InlineType {
    /// No inline DEFINITION (absent, or a bare reference like `struct Node`).
    None,
    /// A named inline definition (`typedef struct Node { … } T;`).
    Named,
    /// An anonymous inline definition, emitted under the alias.
    Anonymous,
}

/// Emits the struct/union/enum DEFINITION carried inline in a typedef's `type`
/// field (`typedef struct Node { int v; } NodeT;`), scoped under the file —
/// exactly what the ObjC walker never did, so the inline type and its fields were
/// dropped (issue #127; the C walker fixed the analog in #107 with
/// `emit_inline_type`).
///
/// Preconditions: `node` is a `typedef_kinds` node; `alias` is the typedef's own
/// name. Postconditions: emits the inner specifier (and its fields/members) iff
/// the `type_field` child is a struct/union/enum specifier WITH a body; emits
/// nothing for a bare reference (`typedef struct Node NodeT;`, no body). Returns
/// which case fired so the caller can suppress the duplicate `Constant` when the
/// anonymous type has taken the alias name.
fn emit_typedef_inline_type(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    alias: &str,
) -> InlineType {
    let Some(inner) = node.child_by_field_name(spec.type_field) else {
        return InlineType::None;
    };
    // Only a DEFINITION (a specifier carrying a body) is emitted, never a bare
    // reference to an existing type — re-emitting the reference would duplicate
    // the real definition's node on its qualified name (#107's lesson).
    if spec
        .body_field
        .and_then(|f| inner.child_by_field_name(f))
        .is_none()
    {
        return InlineType::None;
    }
    let is_anonymous = c_type_name(spec, ctx.source, inner).is_empty();
    let override_name = if is_anonymous && !alias.is_empty() {
        Some(alias)
    } else {
        None
    };
    if kind_in(of.struct_kinds, inner.kind()) {
        emit_c_struct_named(spec, of, ctx, inner, scope, override_name);
    } else if kind_in(of.enum_kinds, inner.kind()) {
        emit_c_enum_named(spec, of, ctx, inner, scope, override_name);
    } else {
        return InlineType::None;
    }
    if override_name.is_some() {
        InlineType::Anonymous
    } else {
        InlineType::Named
    }
}

/// Emits a C `typedef`. The name is the LAST `typedef_name_kind`
/// (`type_identifier`) reached through the `declarator_field` (unwrapping
/// pointer/array declarators), falling back to a full-node DFS.
///
/// The inline struct/union/enum DEFINITION carried in the `type` field is now
/// recursed (issue #127): `typedef struct Node { int v; } NodeT;` emits
/// `Struct|Node` + `Field|v` (`HasField`) as well as `Constant|NodeT`. An
/// ANONYMOUS body (`typedef struct { int v; } T;`) is emitted under the alias and
/// then NO separate `Constant` is emitted — two nodes on one qualified name would
/// be a duplicated key (mirrors the C walker's `emit_typedef`).
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
    if emit_typedef_inline_type(spec, of, ctx, node, scope, &name) == InlineType::Anonymous {
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
