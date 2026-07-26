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

use tree_sitter::Node;

use super::conventions::{
    CallEntry, ClassInheritance, ImportEntry, LanguageConventions, MemberConstant,
};
use super::lang_spec::LangSpec;
use crate::parser::{node_text, qual, Language, LABEL_ENUM, LABEL_STRUCT, LABEL_TRAIT};

// Kotlin-grammar node kinds / fields read only by the conventions (the
// structural ones are in KOTLIN_SPEC and validated by the guard).
// source: tree-sitter-kotlin-ng 1.1.0 node-types.json.
const KT_MODIFIERS: &str = "modifiers";
const KT_INTERFACE_KW: &str = "interface";
const KT_IDENTIFIER: &str = "identifier";
const KT_DELEGATION_SPECIFIERS: &str = "delegation_specifiers";
const KT_NAVIGATION_EXPRESSION: &str = "navigation_expression";
const KT_ENUM_ENTRY: &str = "enum_entry";
const KT_PROPERTY_DECLARATION: &str = "property_declaration";
const KT_VARIABLE_DECLARATION: &str = "variable_declaration";
const KT_MULTI_VARIABLE_DECLARATION: &str = "multi_variable_declaration";

/// Decides whether a call's receiver/qualifier text is safe disambiguating
/// evidence to preserve, or must be discarded down to the bare tail name.
///
/// source: issue #29 — the resolver previously received ONLY `callee_tail`
/// (`rsplit('.').next()`), throwing away every receiver/qualifier and making
/// same-named in-repo calls unresolvable. Preserving it blindly re-introduces a
/// DIFFERENT bug: `viewModel.load()` is a VALUE receiver (a local val/property;
/// this parser has no type information for it), and feeding "viewModel.load"
/// into the resolver's qualified-callee evidence path risks a false string
/// match against an unrelated `load` symbol.
///
/// Kotlin coding conventions (kotlinlang.org/docs/coding-conventions.html):
/// package names are lowercase, class/object names are UpperCamelCase, local
/// vals/vars/properties are lowerCamelCase. A single-dot receiver whose first
/// segment starts lowercase (`viewModel.load`) is therefore always a value/
/// property access, never a package or object qualifier — discard it back to
/// the tail. Two or more dots (`com.foo.bar.process`) or an uppercase-first
/// single dot (`Utils.process`) are real disambiguating evidence — kept as-is.
/// precondition: `tail` is `raw`'s last dot-segment. postcondition: returns
/// `tail` for a value receiver; `raw` otherwise.
fn qualifier_or_tail(raw: &str, tail: &str) -> String {
    let dot_count = raw.matches('.').count();
    // mutation note (§12): the `== 0` → `!= 0` mutant is EQUIVALENT given the
    // precondition (`tail` is `raw`'s last dot-segment): with no dot, `raw ==
    // tail`, so the early return and the fall-through `else` arm return the same
    // string. No precondition-respecting input distinguishes them.
    if dot_count == 0 {
        return tail.to_string();
    }
    let first_seg_uppercase = raw
        .split('.')
        .next()
        .and_then(|s| s.chars().next())
        .map(|c| c.is_uppercase())
        .unwrap_or(false);
    if dot_count == 1 && !first_seg_uppercase {
        tail.to_string()
    } else {
        raw.to_string()
    }
}

/// Kotlin behavioral conventions — the ADR Risk-1 watch surface for phase 4.
pub(super) struct KotlinConventions;

impl KotlinConventions {
    /// First direct child of `node` whose kind is `kind`, or `None`. The cursor
    /// temporary is bound before the return so it drops before the cursor
    /// (E0597).
    fn child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
        let mut cursor = node.walk();
        let found = node.children(&mut cursor).find(|c| c.kind() == kind);
        found
    }

    /// First direct child of `node`, or `None`.
    fn first_child(node: Node) -> Option<Node> {
        let mut cursor = node.walk();
        let found = node.children(&mut cursor).next();
        found
    }

    /// The text of the first direct `identifier` child, or empty. Kotlin's
    /// `class_declaration`/`function_declaration` carry a `name` FIELD (read via
    /// `name_field`), but `enum_entry` does not — its name is an `identifier`
    /// child. (The hand-written walker also checked `simple_identifier`, a kind
    /// that does not exist in tree-sitter-kotlin-ng, so it never matched;
    /// dropped as dead — §9.)
    fn first_identifier(source: &str, node: Node) -> String {
        match Self::child_of_kind(node, KT_IDENTIFIER) {
            Some(id) => node_text(source, id),
            None => String::new(),
        }
    }

    /// The declared name(s) of a `property_declaration`, descended one level
    /// below the direct-child `identifier` scan that `first_identifier` (and the
    /// hand-written walker's `first_identifier`) stops at — the defect #93.
    ///
    /// In tree-sitter-kotlin-ng 1.1.0 a property's name lives inside a
    /// `variable_declaration` child (`property_declaration → variable_declaration
    /// → identifier`), and a destructuring `val (a, b) = …` nests each name a
    /// further level down inside a `multi_variable_declaration`
    /// (`property_declaration → multi_variable_declaration → variable_declaration
    /// → identifier`). The `variable_declaration`'s FIRST direct `identifier` is
    /// the bound name; the property's TYPE (`: Int`) is an `identifier` nested
    /// under a `user_type` grandchild, so `first_identifier`'s direct-child scan
    /// never mistakes it for the name.
    /// source: tree-sitter-kotlin-ng 1.1.0 node-types.json (verified against the
    /// parsed tree of `val x: Int`, `private val breed: String`, `val (a, b)`).
    ///
    /// precondition: `node` is a `property_declaration`. postcondition: one name
    /// per bound identifier, in source order, each non-empty; empty when the
    /// grammar produced no `variable_declaration` (a malformed property).
    fn property_names(source: &str, node: Node) -> Vec<String> {
        // Single binding: `val x = …` / `val x: T = …`.
        if let Some(var_decl) = Self::child_of_kind(node, KT_VARIABLE_DECLARATION) {
            let name = Self::first_identifier(source, var_decl);
            return if name.is_empty() {
                Vec::new()
            } else {
                vec![name]
            };
        }
        // Destructuring: `val (a, b) = …` — each component is its own
        // `variable_declaration` under the `multi_variable_declaration`.
        if let Some(multi) = Self::child_of_kind(node, KT_MULTI_VARIABLE_DECLARATION) {
            let mut cursor = multi.walk();
            return multi
                .children(&mut cursor)
                .filter(|c| c.kind() == KT_VARIABLE_DECLARATION)
                .map(|c| Self::first_identifier(source, c))
                .filter(|n| !n.is_empty())
                .collect();
        }
        Vec::new()
    }

    /// Labels a `class_declaration`/`object_declaration` by content: an
    /// `interface` keyword child → `Trait`; an `enum` class-modifier → `Enum`;
    /// otherwise `Struct` (plain/`data`/`sealed` classes and `object`s).
    ///
    /// tree-sitter-kotlin-ng emits `enum` as a `class_modifier` inside the
    /// `modifiers` child, so a single modifiers check classifies every enum —
    /// empty or not. (The hand-written walker also checked an `enum` keyword and
    /// an `enum_class_body` child; both are redundant with the modifiers signal
    /// in this grammar — verified by the parity corpus's `enum class Color` and
    /// the spec-guard grammar — so they are dropped as dead branches, §9.)
    fn classify_class(source: &str, node: Node) -> &'static str {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let kind = child.kind();
            if kind == KT_INTERFACE_KW {
                return LABEL_TRAIT;
            }
            if kind == KT_MODIFIERS && node_text(source, child).contains("enum") {
                return LABEL_ENUM;
            }
        }
        LABEL_STRUCT
    }

    /// Visibility from the node's `modifiers` child (`public`/`private`/
    /// `protected`/`internal`); Kotlin's default is `public`.
    /// source: kotlinlang.org/docs/visibility-modifiers.html.
    fn visibility_from_modifiers(source: &str, node: Node) -> String {
        if let Some(mods) = Self::child_of_kind(node, KT_MODIFIERS) {
            let t = node_text(source, mods);
            for v in ["public", "private", "protected", "internal"] {
                if t.split_whitespace().any(|w| w == v) {
                    return v.to_string();
                }
            }
        }
        "public".to_string()
    }

    /// The callee `(tail, repr)` for a `call_expression`, or `None` when the
    /// callee is not a bare/qualified identifier (dropped, matching the hand-
    /// written walker). `tail` is the bare identifier (the `CallSite` name);
    /// `repr` is the qualifier-or-tail evidence (the `callee_name` property and
    /// `Calls` target). The callee is the first `navigation_expression` child
    /// (`x.f`) or, failing that, the first child (a bare `identifier`).
    fn callee_parts(source: &str, node: Node) -> Option<(String, String)> {
        let callee_node =
            Self::child_of_kind(node, KT_NAVIGATION_EXPRESSION).or_else(|| Self::first_child(node));
        let callee = callee_node
            .map(|c| node_text(source, c))
            .unwrap_or_default();
        let raw = callee.trim_end_matches('(').to_string();
        let tail = raw.rsplit('.').next().unwrap_or("").to_string();
        // mutation note (§12): the `||` → `&&` mutant on this guard is
        // EQUIVALENT — `tail` is a callee identifier's last dot-segment, so it is
        // either empty (both operands true) or starts with an alphabetic/`_`
        // char (both operands false); a valid identifier cannot start with a
        // digit or symbol, so no input makes the two operands disagree. The `==
        // '_'` char check, by contrast, IS load-bearing (an `_prefixed()` callee
        // — see `kotlin_expression_body_and_edgecase_callees`).
        if tail.is_empty()
            || !tail
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
        {
            return None;
        }
        let repr = qualifier_or_tail(&raw, &tail);
        Some((tail, repr))
    }
}

impl LanguageConventions for KotlinConventions {
    fn visibility_of(&self, _name: &str) -> String {
        // Kotlin visibility is node-based (see `node_visibility`); this name-only
        // entry is reached only via the default `constant_visibility`, which
        // fires for `value_decl_node_kinds` — empty for Kotlin. Returns the
        // `public` default for the trait obligation.
        // mutation note (§12): EQUIVALENT for Kotlin (unreachable path, §9).
        "public".to_string()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // Kotlin methods scope by their enclosing type (`function_node_kinds` +
        // `walk_defs`' `enclosing_class`), not by a receiver node;
        // `method_node_kinds` is empty, so `emit_method_recv` — the only caller —
        // never fires for Kotlin.
        // mutation note (§12): EQUIVALENT for Kotlin (unreachable path, §9).
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        // Overloaded funs share `scope::name`; the per-file `#seq` suffix makes
        // each definition's primary key unique (matching the pre-migration
        // walker), so the walker's collision dedup is a no-op.
        format!("{scope}::{name}#{seq}")
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        // The `CallSite` name is the bare tail identifier; `call_entry` recovers
        // the qualifier-or-tail evidence for the property + edge target.
        Self::callee_parts(source, call_node).map(|(tail, _)| tail)
    }

    fn call_entry(
        &self,
        source: &str,
        call_node: Node,
        caller_qn: &str,
        callee: &str,
        seq: u64,
    ) -> CallEntry {
        // Recompute the qualifier-or-tail evidence (`call_callee` returned only
        // the tail, used as `callee`). Reachable only after `call_callee`
        // accepted the node, so `callee_parts` is `Some`; the `callee` tail is a
        // total fallback.
        let repr = Self::callee_parts(source, call_node)
            .map(|(_, r)| r)
            .unwrap_or_else(|| callee.to_string());
        let line = call_node.start_position().row + 1;
        let col = call_node.start_position().column + 1;
        CallEntry {
            name: callee.to_string(),
            qualified_name: format!("{caller_qn}::call@{line}:{col}#{seq}"),
            visibility: "public".to_string(),
            properties: vec![("callee_name".to_string(), repr.clone())],
            start_line: call_node.start_position().row as u64 + 1,
            end_line: call_node.end_position().row as u64 + 1,
            ref_kind: "Calls",
            ref_to: repr,
        }
    }

    fn imports_of(
        &self,
        source: &str,
        _spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        // `import a.b.C`, `import a.b.C as D`, `import a.b.*`. One statement is
        // one import; strip the keyword and trailing `;`, keep the dotted path
        // verbatim (the resolver keys on the last segment). The display name is
        // the last dotted segment with a trailing `*` stripped (a wildcard's
        // display is empty).
        let text = node_text(source, import_stmt);
        let cleaned = text
            .trim()
            .trim_start_matches("import")
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string();
        if cleaned.is_empty() {
            return Vec::new();
        }
        let display_name = cleaned
            .rsplit('.')
            .next()
            .unwrap_or("")
            .trim_end_matches('*')
            .to_string();
        vec![ImportEntry {
            display_name,
            qualified_name: qual(scope, &format!("import:{cleaned}")),
            ref_to: cleaned.clone(),
            properties: vec![("path".to_string(), cleaned)],
            visibility: "public".to_string(),
            start_line: import_stmt.start_position().row as u64 + 1,
            end_line: import_stmt.end_position().row as u64 + 1,
        }]
    }

    fn node_visibility(&self, source: &str, node: Node, _name: &str) -> String {
        Self::visibility_from_modifiers(source, node)
    }

    fn refine_class_label(
        &self,
        source: &str,
        node: Node,
        _default_label: &'static str,
    ) -> &'static str {
        // `class_declaration`/`object_declaration` carry Trait/Enum/Struct by
        // content, not by node kind (see `classify_class`). `object_declaration`
        // has neither an `interface` nor an `enum` child, so it classifies as
        // `Struct` — matching the hand-written walker.
        Self::classify_class(source, node)
    }

    fn member_constants(&self, source: &str, node: Node) -> Vec<MemberConstant> {
        // Two node kinds route here (`member_constant_kinds`), dispatched by kind:
        match node.kind() {
            // `enum_entry`: an implicitly-public enum member the graph models as a
            // `Constant` marked `enum_entry=true`; its name is a direct identifier
            // child.
            KT_ENUM_ENTRY => {
                let name = Self::first_identifier(source, node);
                if name.is_empty() {
                    return Vec::new();
                }
                vec![MemberConstant {
                    name,
                    visibility: "public".to_string(),
                    properties: vec![("enum_entry".to_string(), "true".to_string())],
                }]
            }
            // `property_declaration` (issue #93): a class-member or top-level
            // `val`/`var`, modelled as a `Constant` with modifier-derived
            // visibility and NO marker property — matching Java's field-as-
            // `Constant` shape (`java.rs`: `field_declaration` → `Constant`,
            // visibility from `modifiers`, empty properties). The name(s) live one
            // level below the direct-child identifier scan, inside a
            // `variable_declaration` (or, for destructuring, each
            // `variable_declaration` under a `multi_variable_declaration`) —
            // `property_names`. All names of one property share its visibility.
            KT_PROPERTY_DECLARATION => {
                let visibility = Self::visibility_from_modifiers(source, node);
                Self::property_names(source, node)
                    .into_iter()
                    .map(|name| MemberConstant {
                        name,
                        visibility: visibility.clone(),
                        properties: Vec::new(),
                    })
                    .collect()
            }
            // Not in `member_constant_kinds`; unreachable via the walker. Returns
            // empty for total-match safety.
            _ => Vec::new(),
        }
    }

    fn class_inheritance(&self, source: &str, _spec: &LangSpec, node: Node) -> ClassInheritance {
        // Supertypes are a `delegation_specifiers` CHILD: a comma list of
        // `Parent()`/`Interface`. Kotlin does not distinguish superclass from
        // interface at parse time, so every supertype is an `Extends` ref (the
        // `()` constructor-call suffix is trimmed). No `bases` property is
        // recorded (parity with the hand-written walker; targets are kept
        // verbatim, the resolver normalizes on lookup).
        let refs: Vec<(&'static str, String)> =
            match Self::child_of_kind(node, KT_DELEGATION_SPECIFIERS) {
                Some(supers) => node_text(source, supers)
                    .split(',')
                    .map(|s| s.trim().trim_end_matches("()"))
                    .filter(|s| !s.is_empty())
                    .map(|s| ("Extends", s.to_string()))
                    .collect(),
                None => Vec::new(),
            };
        ClassInheritance {
            properties: Vec::new(),
            refs,
        }
    }
}

static KOTLIN_CONVENTIONS: KotlinConventions = KotlinConventions;

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

#[cfg(test)]
mod tests {
    use super::qualifier_or_tail;

    /// Pins the issue-#29 receiver/qualifier rule directly. The corpus parity
    /// test covers the bare and value-receiver (lowercase single-dot) cases;
    /// these add the uppercase-object and package-qualified cases that make the
    /// `dot_count == 1 && !first_seg_uppercase` conjunction observably load-
    /// bearing (§12) — under a `&&` → `||` mutant an uppercase single-dot
    /// qualifier would wrongly collapse to its tail.
    #[test]
    fn qualifier_or_tail_rules() {
        // Bare identifier (no dot) → the tail itself.
        assert_eq!(qualifier_or_tail("listOf", "listOf"), "listOf");
        // Value receiver (lowercase, single dot) → discarded to the tail.
        assert_eq!(qualifier_or_tail("viewModel.load", "load"), "load");
        // Object/class qualifier (uppercase-first, single dot) → preserved.
        assert_eq!(qualifier_or_tail("Utils.parse", "parse"), "Utils.parse");
        // Package-qualified (two or more dots) → preserved.
        assert_eq!(
            qualifier_or_tail("com.foo.Helper.build", "build"),
            "com.foo.Helper.build"
        );
    }
}
