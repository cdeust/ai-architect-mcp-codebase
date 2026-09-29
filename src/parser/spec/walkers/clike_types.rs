// parser::spec::walkers::clike_types — the type emitters of the flat C-family
// walker: struct/union with their fields, enum with its entries, typedef, and a
// struct or enum declared inline inside another declaration (issue #107). Split
// out of `clike.rs` so each file stays under the 500-line cap (coding-standards
// §4.1); `clike.rs` keeps the dispatch and the callable emitters. Pure move — no
// behavior changed.

use tree_sitter::Node;

use super::super::lang_spec::CFamilySpec;
use super::clike::CWalk;
use super::declarator::{declarator_field_children, first_identifier, named_or_first_identifier};
use super::{end_line_of, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_ENUM,
    LABEL_FIELD, LABEL_STRUCT,
};

/// Where a type is emitted, and the name an ANONYMOUS specifier takes there
/// (the typedef alias of `typedef struct { … } T;`); `None` keeps the
/// specifier's own name.
#[derive(Clone, Copy)]
pub(super) struct Placement<'a> {
    pub(super) scope: &'a str,
    pub(super) alias: Option<&'a str>,
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

/// Emits a struct/union (`Struct` + `Defines`) and, from its `body_field`, one
/// `Field` + `HasField` per declared member (declarators unwrapped to their
/// field name; a member with no field name — an anonymous member — is skipped).
pub(super) fn emit_struct(w: CWalk, ctx: &mut WalkCtx, node: Node, scope: &str) {
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
pub(super) fn emit_enum(w: CWalk, ctx: &mut WalkCtx, node: Node, scope: &str) {
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
pub(super) enum InlineType {
    /// No inline DEFINITION (absent, or a bare reference like `struct Point`).
    None,
    /// A named inline definition (`typedef struct Tag { … } T;`).
    Named,
    /// An anonymous inline definition, emitted under `alias`.
    Anonymous,
}

pub(super) fn emit_inline_type(
    w: CWalk,
    ctx: &mut WalkCtx,
    node: Node,
    at: Placement,
) -> InlineType {
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
pub(super) fn emit_typedef(w: CWalk, ctx: &mut WalkCtx, node: Node, scope: &str) {
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
