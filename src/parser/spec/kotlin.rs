// parser::spec::kotlin — the Kotlin LangSpec row + KotlinConventions (ADR-0055
// phase 4). Kotlin's tree-sitter-kotlin-ng grammar is a ground-up rewrite whose
// shape diverges from the JVM-sister Java grammar in ways that are STRUCTURAL,
// so they land as gated spec data + a behavioral override rather than as one
// more node-kind slice:
//
//   - One node kind, three concepts. `class_declaration` is `interface`
//     (→ `Trait`), `enum class` (→ `Enum`) and plain/`data`/`sealed`/`object`
//     classes (→ `Struct`), disambiguated by CONTENT not by node kind. That is
//     the `refine_class_label` override, driven by `classify_class`.
//   - Bodies are CHILD nodes, not a `body` field. The grammar has no `body`
//     field at all (its fields are argument/condition/label/left/name/operator/
//     right/type); a class body is a `class_body`/`enum_class_body` child and a
//     function body is a `function_body` child. Hence `class_body_kinds` /
//     `function_body_kinds` (data) and `body_field: None`.
//   - Supertypes are a `delegation_specifiers` CHILD (not an `extends` field),
//     and Kotlin does not split `extends` from `implements` at parse time — all
//     supertypes are one comma list → `Extends` refs. That is the
//     `class_inheritance` override; no `bases` property is recorded (parity with
//     the hand-written walker).
//   - Enum members are `enum_entry` nodes emitted as `Constant`s carrying an
//     `enum_entry=true` marker (NOT `Variant`s), their name read from the
//     entry's identifier child. Properties (`val`/`var`, class-member and
//     top-level) are `property_declaration` nodes emitted as `Constant`s with
//     modifier-derived visibility and no marker (Java's field-as-`Constant`
//     shape), their name(s) descended through the `variable_declaration`
//     child — the fix for issue #93, which the hand-written walker dropped
//     because its `first_identifier` scanned only direct children. Both route
//     through `member_constant_kinds` + `member_constants`.
//   - Visibility is a modifier keyword (`public`/`private`/`protected`/
//     `internal`, default `public`), read off the node — the `node_visibility`
//     override.
//   - Call callees have no `callee` field; the callee is the first expression
//     child (a `navigation_expression` for `x.f()`), reduced to its tail
//     identifier, with the receiver/qualifier kept only when it is real
//     disambiguating evidence (issue #29) — `qualifier_or_tail`.
//
// The size of this override is the ADR Risk-1 watch signal recorded in the PR.
//
// Every node-kind string and field name in KOTLIN_SPEC is real in
// tree-sitter-kotlin-ng's node-types.json (the spec-validation guard asserts
// it). source: tree-sitter-kotlin-ng 1.1.0 src/node-types.json
// (github.com/tree-sitter-grammars/tree-sitter-kotlin, pinned in Cargo.lock).

use super::kotlin_conventions::KOTLIN_CONVENTIONS;
use super::lang_spec::LangSpec;
use crate::parser::Language;

/// The Kotlin language spec row. All node-kind strings and field names:
/// tree-sitter-kotlin-ng 1.1.0 node-types.json (validated by the spec guard).
/// Kotlin populates the child-body fields (`class_body_kinds`,
/// `function_body_kinds`) and the enum-member-as-constant field
/// (`member_constant_kinds`), and leaves the field-based fields
/// (`body_field`, `extends_field`, receiver/type-spec/value-decl/decorator
/// fields) empty/`None` — its inheritance, visibility and class labels are the
/// `KotlinConventions` overrides, not spec slices. `type_field: "type"` is a
/// real Kotlin field kept for the guard; Kotlin never reads it (its type-spec,
/// field and value-decl slices are empty).
pub(crate) static KOTLIN_SPEC: LangSpec = LangSpec {
    language: Language::Kotlin,
    skip_node_kinds: &["package_header"],
    function_node_kinds: &["function_declaration"],
    method_node_kinds: &[],
    class_node_kinds: &["class_declaration", "object_declaration"],
    interface_node_kinds: &[],
    enum_node_kinds: &[],
    variant_node_kinds: &[],
    member_constant_kinds: &["enum_entry", "property_declaration"],
    decorated_def_kinds: &[],
    decorator_node_kind: None,
    base_node_kinds: &[],
    type_decl_node_kinds: &[],
    type_spec_node_kinds: &[],
    struct_type_kind: None,
    interface_type_kind: None,
    field_container_kinds: &[],
    field_node_kinds: &[],
    variable_field_kinds: &[],
    body_wrapper_kinds: &[],
    class_body_kinds: &["class_body", "enum_class_body"],
    function_body_kinds: &["function_body"],
    value_decl_node_kinds: &[],
    value_spec_node_kinds: &[],
    value_name_kind: "identifier",
    variable_declarator_kind: None,
    import_node_kinds: &["import"],
    import_spec_kinds: &[],
    call_node_kinds: &["call_expression"],
    name_field: "name",
    body_field: None,
    type_field: "type",
    receiver_field: None,
    import_path_field: None,
    extends_field: None,
    value_name_field: None,
    value_type_field: None,
    ts_language: || tree_sitter_kotlin_ng::LANGUAGE.into(),
    embedded: &[],
    conventions: &KOTLIN_CONVENTIONS,
    c_family: None,
    cpp_family: None,
    objc_family: None,
    ts_family: None,
    ts_language_by_ext: None,
    rust_family: None,
};
