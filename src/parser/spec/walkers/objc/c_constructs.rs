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
// under the declarator. Those differences are preserved here — they are ObjC's
// name resolution, not a defect.
//
// What WAS a defect is that this lane never looked at a typedef's inline type
// definition, so `typedef struct Node { int v; } NodeT;` contributed only the
// alias `Constant` and dropped the struct and its fields (issue #127 — the same
// gap #107 closed for C). The DECISION that closes it — "does this declaration's
// `type` field define a type, or merely name one?" — is the shared
// `inline_type` helper, so this lane reuses #107's guard rather than
// reimplementing it; only the emission below is ObjC's.

use tree_sitter::Node;

use super::super::super::family_specs::ObjcFamilySpec;
use super::super::super::lang_spec::LangSpec;
use super::super::declarator::first_field_identifier;
use super::super::inline_type::{defines_a_type, inline_type_definition, InlineName};
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
/// dead code, so it is removed rather than pinned (§9, §12.1).
/// source: tree-sitter-objc 3.0.2 node-types.json (struct_specifier /
/// union_specifier / enum_specifier: fields `name`/`body`, children
/// `attribute_specifier`/`ms_declspec_modifier`; enumerator: field `name`).
fn c_type_name(spec: &LangSpec, source: &str, node: Node) -> String {
    node_field_text(source, node, spec.name_field)
}

/// Emits a C `struct`/`union` as a `Struct` + `Defines` and, from its
/// `body_field`, one `Field` + `HasField` per declared member.
///
/// Preconditions: `node`'s kind is in `of.struct_kinds`. Postconditions: one
/// `Struct` + one `Defines` plus one `Field` + `HasField` per member, or NOTHING
/// when the specifier defines no type (a forward declaration) or resolves to no
/// name (`override_name` covers the anonymous-inside-a-typedef case).
///
/// The bodiless guard is the shared `defines_a_type`: `struct Point;` is the same
/// node kind as the definition, and emitting it put a second one-line `Struct` on
/// the definition's qualified name — measured on
/// `struct Point;\nstruct Point { int x; };` before this guard (boy-scout, §14).
pub(super) fn emit_c_struct(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    override_name: Option<&str>,
) {
    if !defines_a_type(spec, node) {
        return;
    }
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
            let fname = first_field_identifier(of.field_identifier_kind, ctx.source, declarator);
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

/// Emits a C `enum` as an `Enum` + `Defines` and its `enum_member_kinds` entries
/// as `Constant`s (`enum_entry=true`) + `Defines` scoped under the enum.
///
/// Preconditions: `node`'s kind is in `of.enum_kinds`. Postconditions: one `Enum`
/// plus one `Constant` per named member, or NOTHING for a bodiless specifier
/// (`enum E;`) or an unresolvable name. Same `defines_a_type` guard, same reason,
/// as `emit_c_struct`.
pub(super) fn emit_c_enum(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    override_name: Option<&str>,
) {
    if !defines_a_type(spec, node) {
        return;
    }
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

/// Emits a C `typedef` as a `Constant` (`typedef=true`) + `Defines`, AND the type
/// it defines inline (issue #127). The alias name is the LAST `typedef_name_kind`
/// (`type_identifier`) reached through the `declarator_field` (unwrapping
/// pointer/array declarators), falling back to a full-node DFS.
///
/// Preconditions: `node`'s kind is in `of.typedef_kinds`. Postconditions: the
/// inline type definition (if any) is emitted with its members, and exactly one
/// node carries the alias name — either the alias `Constant`, or the ANONYMOUS
/// type emitted under the alias, never both.
///
/// The three cases, all one node kind in the grammar:
///   - `typedef struct Node { int v; } NodeT;` → `Struct|Node` + `Field|v` AND
///     `Constant|NodeT`: the tag and the alias are two genuine names.
///   - `typedef struct { int a; } AnonT;`      → `Struct|AnonT` + `Field|a` and NO
///     alias `Constant`: the alias is the only name that type has, and emitting
///     both would put two nodes on one qualified name (a duplicated primary key).
///   - `typedef struct Node OtherT;`           → only `Constant|OtherT`: this
///     REFERENCES an existing type, so re-emitting `Node` would duplicate it.
///     That last case is why the decision is `inline_type_definition`'s
///     body-presence test and not "does a `type` field exist".
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
    if emit_inline_type(spec, of, ctx, node, scope, &name) == Some(InlineName::Anonymous) {
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

/// Emits the type a typedef defines INLINE in its `type_field`, scoped under
/// `scope` (issue #127).
///
/// Preconditions: `node` is a typedef; `alias` is its resolved alias name.
/// Postconditions: returns `Some(InlineName::Anonymous)` exactly when the type was
/// emitted UNDER the alias — the signal the caller needs to suppress a second node
/// on that qualified name — `Some(Named)` when a tagged type was emitted under its
/// own tag, and `None` when the typedef defines no type inline (a bare reference,
/// or a non-struct/enum type).
///
/// This mirrors `clike::emit_inline_type` in shape because both answer the same
/// question, but the emission cannot be shared: ObjC resolves C type names by the
/// `name` field then the first BARE `identifier`, and models members through its
/// own `emit_c_struct_fields`. The shared part — the body-presence decision that
/// distinguishes a definition from a reference — IS shared, via
/// `inline_type_definition`.
fn emit_inline_type(
    spec: &LangSpec,
    of: &ObjcFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
    alias: &str,
) -> Option<InlineName> {
    let (inner, name_kind) = inline_type_definition(spec, node)?;
    let override_name = if name_kind == InlineName::Anonymous && !alias.is_empty() {
        Some(alias)
    } else {
        None
    };
    let k = inner.kind();
    if kind_in(of.struct_kinds, k) {
        emit_c_struct(spec, of, ctx, inner, scope, override_name);
    } else if kind_in(of.enum_kinds, k) {
        emit_c_enum(spec, of, ctx, inner, scope, override_name);
    } else {
        return None;
    }
    if override_name.is_some() {
        Some(InlineName::Anonymous)
    } else {
        Some(InlineName::Named)
    }
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
