// parser::spec::walkers::inline_type — the ONE "is this a type DEFINITION or just
// a reference to one?" test, shared by all three C-family lanes (`clike` for C,
// `cpp`, `objc`).
//
// The trap, paid for three times: a C-family grammar uses the SAME node kind for
// a definition and for a bare reference / forward declaration.
//
//     struct Point { int x; };                // struct_specifier WITH a body
//     struct Point;                           // struct_specifier, NO body
//     typedef struct Node { int v; } NodeT;   // definition inside a typedef
//     typedef struct Node OtherT;             // reference inside a typedef
//
// Emitting a bodiless specifier produces a SECOND node on a qualified name the
// real definition already owns, whose line span is the one-line declaration —
// measured in all three lanes before this guard existed. Never looking at a
// declaration's `type` field instead drops an inline definition and its fields
// (issue #107 for C, #127 for ObjC).
//
// Both halves are the same question, so it is asked in one place and each lane
// supplies only its own EMISSION (the label, the name resolution, and the scope
// genuinely differ per lane — that is why the emitters are not shared).
//
// Reads only `spec.type_field`, `spec.body_field`, and `spec.name_field`, all
// validated against each grammar's node-types.json by the spec guard.
// source: tree-sitter-c 0.23.4, tree-sitter-cpp 0.23.4, tree-sitter-objc 3.0.2
// node-types.json — in all three, struct_specifier/enum_specifier declare `body`
// and `name` as OPTIONAL fields, which is precisely why presence must be tested.

use tree_sitter::Node;

use super::super::lang_spec::LangSpec;

/// Whether `specifier` DEFINES a type here, rather than naming one defined
/// elsewhere: true iff it carries a `body_field` child.
///
/// Preconditions: `specifier` is a struct/union/enum/class specifier node.
/// Postconditions: false for a forward declaration (`struct Point;`), an opaque
/// enum declaration (`enum class E : int;`), a bare reference inside a typedef,
/// and for any grammar that names no `body_field` at all.
///
/// Every emitter that turns a specifier into a graph node calls this FIRST. That
/// placement is deliberate: enforcing it at the point of emission covers every
/// dispatch route into that emitter (top level, a class body, a typedef's `type`
/// field) instead of once per route, which is how the same defect reappeared in
/// each lane.
pub(super) fn defines_a_type(spec: &LangSpec, specifier: Node) -> bool {
    spec.body_field
        .and_then(|f| specifier.child_by_field_name(f))
        .is_some()
}

/// Whether an inline type definition carries its own name, or only the alias of
/// the declaration wrapping it.
#[derive(PartialEq, Eq, Debug)]
pub(super) enum InlineName {
    /// `typedef struct Tag { … } T;` — the specifier has a `name_field`, so the
    /// tag and the alias are genuinely two names.
    Named,
    /// `typedef struct { … } T;` — the specifier is anonymous, so the wrapping
    /// declaration's alias is the ONLY usable name that type has.
    Anonymous,
}

/// The type DEFINITION declared inline in `decl`'s `type_field`, or `None`.
///
/// Preconditions: `decl` is a declaration that may carry a type in `type_field`
/// (a typedef, a variable declaration, a class member).
/// Postconditions: `Some((specifier, name_kind))` only when the `type_field`
/// child satisfies `defines_a_type`; `None` for an absent type field or a
/// bodiless specifier.
///
/// Returning the specifier rather than emitting anything keeps the
/// language-specific part with the caller.
pub(super) fn inline_type_definition<'t>(
    spec: &LangSpec,
    decl: Node<'t>,
) -> Option<(Node<'t>, InlineName)> {
    let inner = decl.child_by_field_name(spec.type_field)?;
    if !defines_a_type(spec, inner) {
        return None;
    }
    let name_kind = match inner.child_by_field_name(spec.name_field) {
        Some(_) => InlineName::Named,
        None => InlineName::Anonymous,
    };
    Some((inner, name_kind))
}
