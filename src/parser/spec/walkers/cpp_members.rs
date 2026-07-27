// parser::spec::walkers::cpp_members — what a C++ scope CONTAINS: enums and
// their members, method prototypes, data fields, nested type definitions,
// typedefs, and alias declarations. The dispatch and the scope containers
// (namespace / class / function definition) live in the sibling `cpp` module.
//
// This module is where issue #124's five extraction-shape gaps are closed:
//   1. enum members    → `emit_enum` recurses `enum_member_kinds`
//   2. ctors / dtors   → `emit_member` reached via `member_decl_kinds`
//   3. `using X = Y;`  → `emit_alias` → `TypeAlias`
//   4. data members    → `emit_member_field` → `Field` + `HasField`
//   5. out-of-body     → in `cpp::emit_function` (it is a definition, not a member)
//
// `emit_member` is the single entry point for both member node kinds, deciding
// per DECLARED NAME whether it is a prototype, a data field, or (from the `type`
// field) a nested type — the three shapes share one node kind and one declaration
// can mix them.
//
// Every node kind and field: tree-sitter-cpp 0.23.4 src/node-types.json,
// validated by the spec guard.

use tree_sitter::Node;

use super::super::lang_spec::{CppFamilySpec, LangSpec};
use super::cpp::emit_class_like;
use super::declarator::{
    binds_function_prototype, declarator_field_children, declarator_name, named_or_first_identifier,
};
use super::{end_line_of, kind_in, line_of, WalkCtx};
use crate::parser::{
    node_field_text, qual, ExtractedNode, ExtractedRef, LABEL_CONSTANT, LABEL_ENUM, LABEL_FIELD,
    LABEL_METHOD, LABEL_TYPE_ALIAS,
};

/// Emits an enum (`Enum` + `Defines`) and, from its `body_field`, one `Constant`
/// (`enum_entry=true`) + `Defines` per `enum_member_kinds` entry scoped under the
/// enum — the same shape the flat C walker emits (issue #124 item 1; the
/// pre-#124 walker emitted the bare `Enum` and dropped every member).
///
/// Preconditions: `node`'s kind is in `cf.enum_kinds`. Postconditions: one `Enum`
/// node plus one `Constant` per named member, each with a `Defines` edge from its
/// container; NOTHING when the specifier has no body or no resolvable name.
///
/// The body-presence guard matches `emit_class_like`'s and exists for the same
/// reason: an opaque enum declaration (`enum class Status : int;`) is the same
/// node kind as the definition, and emitting it duplicated the definition's QN.
///
/// `enum class Status { … }` is the SAME `enum_specifier` kind as a plain `enum`
/// (verified against node-types.json), so both surface identically. A member's
/// name is its `name_field` — NOT an identifier search — so `GREEN = 5` resolves
/// to `GREEN` and a computed initializer (`B = f(x)`) cannot capture the name.
pub(super) fn emit_enum(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    let Some(body) = spec.body_field.and_then(|f| node.child_by_field_name(f)) else {
        return;
    };
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
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if !kind_in(cf.enum_member_kinds, child.kind()) {
            continue;
        }
        let member = node_field_text(ctx.source, child, spec.name_field);
        if member.is_empty() {
            continue;
        }
        let member_qn = qual(&qn, &member);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_CONSTANT.to_string(),
            name: member.clone(),
            qualified_name: member_qn.clone(),
            start_line: line_of(child),
            end_line: end_line_of(child),
            visibility: spec.conventions.visibility_of(&member),
            properties: vec![("enum_entry".to_string(), "true".to_string())],
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: qn.clone(),
            to_qualified_name: member_qn,
        });
    }
}

/// Emits one member of a class/struct/union body, routing EACH declared name on
/// what that name actually declares.
///
/// Preconditions: `node`'s kind is in `cf.field_decl_kinds` or
/// `cf.member_decl_kinds`, and `scope` is the enclosing type's QN.
/// Postconditions: one node per declared name — a prototype `Method` for a name
/// bound by a function declarator, a `Field` otherwise — plus the nested type when
/// the member's `type_field` is an inline type DEFINITION. A member that binds no
/// name and declares no type body emits nothing.
///
/// The shapes are disjoint per NAME but share one node kind, and one declaration
/// can mix them (`int x, f();`), which is why the decision is per declarator
/// rather than per node — a node-wide test would label `x` a method or drop `f`:
///   - `int f() const;` / `Point(int,int);` → function declarator ⇒ prototype
///   - `int a, b;` / `int& r;`              → plain declarators   ⇒ data members
///   - `void (*cb)(int);`                   → function POINTER     ⇒ data member
///     (issue #135: a `pointer_declarator` sits between the outer
///     `function_declarator` and the name, so it is data, not a callable)
///   - `class Inner { int z; };`            → `type` specifier    ⇒ nested type
pub(super) fn emit_member(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    emit_nested_type(spec, cf, ctx, node, scope);
    let type_text = node_field_text(ctx.source, node, spec.type_field);
    for declarator in declarator_field_children(cf.naming, node) {
        let name = declarator_name(cf.naming, ctx.source, declarator);
        if name.is_empty() {
            continue;
        }
        if binds_function_prototype(cf.naming, cf.func_declarator_kind, declarator) {
            emit_member_proto(spec, ctx, node, scope, &name);
        } else {
            emit_member_field(spec, cf, ctx, node, scope, &name, &type_text);
        }
    }
}

/// Emits a type declared INLINE in a member's `type_field`
/// (`class Inner { int z; };`, `enum E { A };` inside a class body), scoped under
/// the enclosing type.
///
/// Preconditions: `node` is a member declaration. Postconditions: emits the
/// nested type (and recursively its own members) iff the `type_field` child is a
/// class/struct/union/enum specifier with a body; emits nothing otherwise.
///
/// A member whose `type_field` is a bare forward declaration (`class Inner;`) is
/// the same node shape as a definition, and must contribute nothing — the
/// body-presence guard that enforces that lives in the two emitters below, so
/// this dispatch does not repeat it (#107's lesson, one implementation). Pinned by
/// `cpp_forward_declaration_in_a_class_body_emits_nothing`.
fn emit_nested_type(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    let Some(inner) = node.child_by_field_name(spec.type_field) else {
        return;
    };
    let k = inner.kind();
    if kind_in(cf.class_kinds, k) {
        emit_class_like(spec, cf, ctx, inner, scope, /*is_class=*/ true);
    } else if kind_in(cf.struct_kinds, k) {
        emit_class_like(spec, cf, ctx, inner, scope, /*is_class=*/ false);
    } else if kind_in(cf.enum_kinds, k) {
        emit_enum(spec, cf, ctx, inner, scope);
    }
}

/// Emits one data member as a `Field` + `HasField`, carrying the declaration's
/// `type_field` text as a `type_annotation` — the flat C-family model (issue #124
/// item 4; the pre-#124 walker emitted a `Constant` + `Defines` named after the
/// deepest-rightmost identifier in the whole declaration).
///
/// Preconditions: `name` is non-empty and was bound by one of `node`'s declarators
/// without a function declarator; `owner_qn` is the enclosing type's QN.
/// Postconditions: exactly one `Field` node and one `HasField` edge.
///
/// The line span is the DECLARATION's, not the declarator's, so both names of
/// `int a, b;` report the line the reader sees.
fn emit_member_field(
    spec: &LangSpec,
    _cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    owner_qn: &str,
    name: &str,
    type_text: &str,
) {
    let qn = qual(owner_qn, name);
    let mut props = Vec::new();
    if !type_text.is_empty() {
        props.push(("type_annotation".to_string(), type_text.to_string()));
    }
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FIELD.to_string(),
        name: name.to_string(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(name),
        properties: props,
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasField".to_string(),
        from_qualified_name: owner_qn.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits a member function prototype (`Method`, `is_prototype=true`, receiver =
/// the enclosing type) + `HasMethod`, keyed `{scope}::{name}#{seq}`. No body ⇒ no
/// calls.
///
/// Preconditions: `name` is non-empty and was bound by one of `node`'s function
/// declarators, inside a class body; `scope` is the enclosing type's QN.
/// Postconditions: exactly one `Method` node and one `HasMethod` edge. Invariant:
/// one `seq` per emitted node, keeping the single per-file ordering.
///
/// The name is what the DECLARATOR binds (issue #123), which is what makes the
/// three member spellings come out right:
///   - `virtual double area() const;`  → `area`      (a `field_identifier`)
///   - `Shape operator+(const Shape&)` → `operator+` (an `operator_name`, taken
///     as its own text — its inner `identifier` child holds nothing usable)
///   - `~Point();`                     → `~Point`    (a `destructor_name`, taken
///     as its own text — its inner `identifier` is `Point`, which would collide
///     with the constructor and with the class itself)
fn emit_member_proto(spec: &LangSpec, ctx: &mut WalkCtx, node: Node, scope: &str, name: &str) {
    let seq = ctx.next_seq();
    let qn = spec.conventions.def_qn(scope, name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.to_string(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(name),
        properties: vec![
            ("is_prototype".to_string(), "true".to_string()),
            ("receiver_type".to_string(), scope.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Emits a typedef as a `Constant` (`typedef=true`) + `Defines`. The name is the
/// first identifier leaf of the whole `type_definition` (the LIFO DFS lands on
/// the declared alias, which follows the aliased type in child order).
///
/// Preconditions: `node`'s kind is in `cf.typedef_kinds`. Postconditions: one
/// `Constant` + one `Defines`, or nothing when no identifier leaf exists.
///
/// A `typedef` and a `using` alias mean the same thing in C++ but are modeled
/// differently here — `Constant` vs `TypeAlias`. Issue #124 item 3 asked only for
/// the alias-declaration (which was dropped entirely); re-labelling the typedef is
/// a separate, consumer-visible decision and is deliberately NOT bundled in.
pub(super) fn emit_typedef(
    spec: &LangSpec,
    cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    let name = super::declarator::first_identifier(cf.naming, ctx.source, node);
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

/// Emits an alias declaration (`using Distance = double;`) as a `TypeAlias` +
/// `Defines`, carrying the aliased type as a `type_annotation` property (issue
/// #124 item 3; the pre-#124 walker matched only `using_declaration` and dropped
/// `alias_declaration` entirely).
///
/// Preconditions: `node`'s kind is in `cf.alias_kinds`. Postconditions: one
/// `TypeAlias` + one `Defines`, or nothing when the `name_field` is absent.
///
/// `TypeAlias` is the label this graph already uses for exactly this construct in
/// TypeScript (`type X = Y`), Rust (`type X = Y;`) and Go type aliases, so a C++
/// alias is queryable the same way rather than under a C++-only shape.
/// `using namespace std;` is a DIFFERENT node kind (`using_declaration`) and keeps
/// its `Import` shaping — pinned by `cpp_using_declaration_stays_an_import`.
pub(super) fn emit_alias(
    spec: &LangSpec,
    _cf: &CppFamilySpec,
    ctx: &mut WalkCtx,
    node: Node,
    scope: &str,
) {
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let aliased = node_field_text(ctx.source, node, spec.type_field);
    let mut props = Vec::new();
    if !aliased.is_empty() {
        props.push(("type_annotation".to_string(), aliased));
    }
    ctx.nodes.push(ExtractedNode {
        label: LABEL_TYPE_ALIAS.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: line_of(node),
        end_line: end_line_of(node),
        visibility: spec.conventions.visibility_of(&name),
        properties: props,
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}
