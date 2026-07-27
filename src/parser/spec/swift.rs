// parser::spec::swift — the Swift LangSpec row + SwiftConventions (ADR-0055
// phase 5). tree-sitter-swift's grammar diverges from the JVM-family grammars
// in ways that are STRUCTURAL, so they land as gated spec data + a behavioral
// override rather than as one more node-kind slice:
//
//   - One node kind, several concepts. `class_declaration` is the umbrella for
//     `class`, `struct`, `actor`, `enum` and `extension`, disambiguated by the
//     `declaration_kind` FIELD (values: class | struct | actor | enum |
//     extension), not by node kind. `enum` → `Enum`; everything else → `Struct`.
//     That is the `refine_class_label` override. Superclass + protocol
//     conformances are `inheritance_specifier` children (each naming its
//     supertype in an `inherits_from` field); Swift, like Kotlin, does not split
//     the superclass from the conformance list at parse time, so every entry is
//     an `Extends` ref (issue #97). That is the `class_inheritance` override; an
//     `extension` additionally carries an `is_extension=true` property.
//   - `init` / `deinit` / `subscript` are their OWN declaration kinds with no
//     usable `name` field; the graph names them synthetically (`init`/`deinit`/
//     `subscript`) and marks them with a `member_kind` property. They route
//     through `function_node_kinds` (so they become `Method`/`Function` like any
//     def) with the `def_name` + `function_props` overrides supplying the name
//     and the marker. `subscript_declaration` has no `body` field — its body is
//     a `computed_property` child — so `function_body_kinds` names that fallback.
//   - Protocol requirements use DISTINCT node kinds inside a `protocol_body`:
//     `protocol_function_declaration` (→ `Method`/`HasMethod` via
//     `function_node_kinds`, bodiless so no calls scanned) and
//     `protocol_property_declaration` (→ `Constant`/`Defines` via
//     `member_constant_kinds`). Both are extracted (issue #98).
//   - Enum cases are `enum_entry` nodes → `Variant`s edged `HasVariant` with
//     `public` visibility (the canonical model, aligned with Java's
//     `enum_constant`; issue #99), and one node may bind several names
//     (`case green, blue`) via the generic multi-name emit.
//   - Properties and typealiases are `Constant`s (Swift has no separate constant
//     concept in this graph). They route through `member_constant_kinds` with a
//     `member_constants` override reading the declaration's name and modifier
//     visibility (typealiases additionally carry `typealias=true`). A COMPUTED
//     property's getter/setter/observer body is scanned for calls, keyed by the
//     property's QN — the `member_constant_call_body` override (issue #100).
//   - Visibility is a modifier keyword (`public`/`private`/`internal`/
//     `fileprivate`/`open`, default `internal`) read off the node head — the
//     `node_visibility` override.
//   - Calls reduce the callee to its trailing dotted segment (`Utils.parse` →
//     `parse`), with no qualifier preserved — `call_callee`/`call_entry`.
//
// The size of this override is the ADR Risk-1 watch signal recorded in the PR.
//
// Every node-kind string and field name in SWIFT_SPEC is real in
// tree-sitter-swift's node-types.json (the spec-validation guard asserts it).
// source: alex-pinkus/tree-sitter-swift 0.7.3 src/node-types.json (pinned in
// Cargo.lock; ABI-15, do not bump).

use tree_sitter::Node;

use super::conventions::{
    CallEntry, ClassInheritance, ImportEntry, LanguageConventions, MemberConstant,
};
use super::lang_spec::LangSpec;
use crate::parser::{node_field_text, node_text, qual, Language, LABEL_ENUM};

// Swift-grammar node kinds / fields read only by the conventions (the structural
// ones are in SWIFT_SPEC and validated by the guard).
// source: alex-pinkus/tree-sitter-swift 0.7.3 node-types.json.
const SW_INIT_DECL: &str = "init_declaration";
const SW_DEINIT_DECL: &str = "deinit_declaration";
const SW_SUBSCRIPT_DECL: &str = "subscript_declaration";
const SW_PROPERTY_DECL: &str = "property_declaration";
const SW_PROTOCOL_PROPERTY_DECL: &str = "protocol_property_declaration";
const SW_TYPEALIAS_DECL: &str = "typealias_declaration";
const SW_DECLARATION_KIND: &str = "declaration_kind";
const SW_EXTENSION: &str = "extension";
const SW_ENUM: &str = "enum";
// Conformance/inheritance: `class_declaration`/`protocol_declaration` carry
// zero-or-more `inheritance_specifier` children, each naming its supertype in an
// `inherits_from` field (a `user_type`). Computed-accessor bodies: a
// `property_declaration`'s getter/setter live under its `computed_value` field
// (a `computed_property`), and stored-property observers in a
// `willset_didset_block` child.
// source: alex-pinkus/tree-sitter-swift 0.7.3 node-types.json.
const SW_INHERITANCE_SPECIFIER: &str = "inheritance_specifier";
const SW_INHERITS_FROM: &str = "inherits_from";
const SW_COMPUTED_VALUE: &str = "computed_value";
const SW_WILLSET_DIDSET: &str = "willset_didset_block";
const SW_NAME: &str = "name";
const SW_BOUND_IDENTIFIER: &str = "bound_identifier";

/// Swift behavioral conventions — the ADR Risk-1 watch surface for phase 5.
pub(super) struct SwiftConventions;

impl SwiftConventions {
    /// Visibility from the modifier keyword on the declaration's head line
    /// (`public`/`private`/`internal`/`fileprivate`/`open`); Swift's default is
    /// `internal`. Reads the head line rather than a `modifiers` child so an
    /// attributed/leading-comment declaration classifies on the keyword actually
    /// governing the declaration — matching the hand-written walker.
    /// source: swift.org/documentation — access control default is `internal`.
    fn visibility_modifier(source: &str, node: Node) -> String {
        let text = node_text(source, node);
        let head = text.lines().next().unwrap_or("");
        for v in ["public", "private", "internal", "fileprivate", "open"] {
            if head.split_whitespace().any(|w| w == v) {
                return v.to_string();
            }
        }
        "internal".to_string()
    }

    /// The clean bound name of a declaration whose `name` field is a `pattern`
    /// that embeds its `var`/`let` binding (a `protocol_property_declaration`):
    /// the pattern's `bound_identifier` field (`var id` → `id`). Empty when the
    /// pattern or its `bound_identifier` is absent.
    /// source: alex-pinkus/tree-sitter-swift 0.7.3 node-types.json
    /// (`pattern.bound_identifier: simple_identifier`).
    fn pattern_bound_identifier(source: &str, node: Node) -> String {
        match node.child_by_field_name(SW_NAME) {
            Some(pattern) => node_field_text(source, pattern, SW_BOUND_IDENTIFIER),
            None => String::new(),
        }
    }

    /// The `declaration_kind` field's text (`class`/`struct`/`actor`/`enum`/
    /// `extension` for a `class_declaration`; `protocol` for a
    /// `protocol_declaration`), trimmed. Empty if the field is absent.
    fn declaration_kind(source: &str, node: Node) -> String {
        node_field_text(source, node, SW_DECLARATION_KIND)
            .trim()
            .to_string()
    }

    /// The `(tail, valid)` callee for a `call_expression`: the first named
    /// child's text reduced to its trailing dotted segment with a trailing `(`
    /// trimmed (`Utils.parse` → `parse`, `items.map` → `map`), and whether it is
    /// a usable identifier (non-empty, alphabetic-or-`_` first char). Mirrors the
    /// hand-written walker exactly — Swift preserves no qualifier.
    fn callee_tail(source: &str, node: Node) -> Option<String> {
        let callee_text = node
            .named_child(0)
            .map(|c| node_text(source, c))
            .unwrap_or_default();
        let tail = callee_text
            .rsplit('.')
            .next()
            .unwrap_or("")
            .trim_end_matches('(')
            .trim()
            .to_string();
        // mutation note (§12): the `|| c == '_'` char-class arm is load-bearing —
        // a `_prefixed()` callee is admitted only by it (asserted by the parity
        // corpus's underscore-callee edge pin). The `is_empty` arm guards the
        // no-named-child case.
        if !tail.is_empty()
            && tail
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
        {
            Some(tail)
        } else {
            None
        }
    }
}

impl LanguageConventions for SwiftConventions {
    fn visibility_of(&self, _name: &str) -> String {
        // Swift visibility is node-based (see `node_visibility`); this name-only
        // entry is unreachable for Swift (no `type_decl`/`field`/`value_decl`
        // slices, and `constant_visibility` is bypassed by `emit_member_constant`
        // using the `member_constant` visibility). Returns the `internal`
        // default for the trait obligation.
        // mutation note (§12): EQUIVALENT for Swift (unreachable path, §9).
        "internal".to_string()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // Swift methods scope by their enclosing type (`function_node_kinds` +
        // `walk_defs`' `enclosing_class`), not by a receiver node;
        // `method_node_kinds` is empty, so `emit_method_recv` never fires.
        // mutation note (§12): EQUIVALENT for Swift (unreachable path, §9).
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        // Every Swift function/method (incl. init/deinit/subscript) is keyed by a
        // per-file `#seq` suffix (matching the hand-written walker), so the
        // walker's collision dedup is a no-op.
        format!("{scope}::{name}#{seq}")
    }

    fn def_name(&self, source: &str, node: Node, name_field: &str) -> String {
        // `deinit`/`subscript` have no usable `name` field; the graph names them
        // synthetically (the hand-written walker's `synth_name`). `init` needs no
        // arm — `init_declaration`'s `name` field IS the literal token `init`
        // (node-types.json: `name` types = ["init"]), so the default path already
        // yields "init". Every other function-like node (`function_declaration`,
        // whose `name` field is always present) also takes the default path.
        // (§9: no redundant init arm — a `delete init arm` mutant would be
        // equivalent, so the arm is not written.)
        match node.kind() {
            SW_DEINIT_DECL => "deinit".to_string(),
            SW_SUBSCRIPT_DECL => "subscript".to_string(),
            _ => node_field_text(source, node, name_field),
        }
    }

    fn function_props(&self, _source: &str, node: Node) -> Vec<(String, String)> {
        // init/deinit/subscript carry a `member_kind` marker (the hand-written
        // walker's property); a plain `function_declaration` carries none. Emitted
        // BEFORE the walker appends `receiver_type`, matching the hand-written
        // order `[member_kind, receiver_type]`.
        let member_kind = match node.kind() {
            SW_INIT_DECL => "init",
            SW_DEINIT_DECL => "deinit",
            SW_SUBSCRIPT_DECL => "subscript",
            _ => return Vec::new(),
        };
        vec![("member_kind".to_string(), member_kind.to_string())]
    }

    fn node_visibility(&self, source: &str, node: Node, _name: &str) -> String {
        Self::visibility_modifier(source, node)
    }

    // #99: enum cases use the canonical `HasVariant`/`public` model — Swift no
    // longer overrides `variant_edge_kind`/`variant_visibility`, so it inherits
    // the trait defaults, aligning `enum_entry` variants with Java's
    // `enum_constant` (conventions.rs defaults; java.rs `variant_node_kinds`).
    // The old `Defines`/`internal` overrides (the hand-written walker's choice,
    // preserved by #102) are removed.

    fn refine_class_label(
        &self,
        source: &str,
        node: Node,
        default_label: &'static str,
    ) -> &'static str {
        // `class_declaration` is class/struct/actor/enum/extension by content;
        // only `enum` changes the label (→ `Enum`), the rest are `Struct`
        // (the `default_label` `class_like_label` already chose). A
        // `protocol_declaration` (default_label `Trait`, declaration_kind
        // `protocol`) keeps its label — only the `enum` keyword refines.
        if Self::declaration_kind(source, node) == SW_ENUM {
            LABEL_ENUM
        } else {
            default_label
        }
    }

    fn class_inheritance(&self, source: &str, _spec: &LangSpec, node: Node) -> ClassInheritance {
        // #97: superclass + protocol conformances are `inheritance_specifier`
        // children (a `class`/`struct`/`enum`/`actor`/`extension`/`protocol` may
        // carry several), each naming its supertype in an `inherits_from` field.
        // Swift, like Kotlin, does NOT split the superclass from the protocol
        // list at parse time — the grammar gives one undifferentiated
        // `inheritance_specifier` sequence — so every entry is an `Extends` ref,
        // matching KotlinConventions::class_inheritance (kotlin.rs). No `bases`
        // property is recorded (Kotlin parity; targets kept verbatim, the
        // resolver normalizes on lookup — java.rs/kotlin.rs precedent). An
        // `extension` additionally carries an `is_extension=true` property so
        // downstream tooling can merge it into the extended type.
        let mut refs: Vec<(&'static str, String)> = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == SW_INHERITANCE_SPECIFIER {
                let name = node_field_text(source, child, SW_INHERITS_FROM);
                let name = name.trim();
                if !name.is_empty() {
                    refs.push(("Extends", name.to_string()));
                }
            }
        }
        let properties = if Self::declaration_kind(source, node) == SW_EXTENSION {
            vec![("is_extension".to_string(), "true".to_string())]
        } else {
            Vec::new()
        };
        ClassInheritance { properties, refs }
    }

    fn member_constants(&self, source: &str, node: Node) -> Vec<MemberConstant> {
        // Routed for `property_declaration` (a `let`/`var`, computed or stored →
        // `Constant`), `typealias_declaration` (→ `Constant` marked
        // `typealias=true`), and `protocol_property_declaration` (a protocol
        // property requirement `var x: T { get }` → `Constant`, issue #98). All
        // take modifier visibility. The hand-written walker read a SINGLE name
        // (its `find_name`, the pattern's first name), so a Swift property binds
        // exactly one `Constant` (parity), never several (unlike Kotlin's
        // destructuring `val (a, b)`). An empty name is skipped by
        // `emit_member_constant`.
        let (name, properties) = match node.kind() {
            // A class property's `name` field is a bare `pattern` (its `var`/`let`
            // is a separate `value_binding_pattern` child), so its text IS the
            // name. A protocol property requirement embeds the binding INSIDE the
            // pattern (`var id` — no separate child), so its clean name is the
            // pattern's `bound_identifier` field.
            SW_PROPERTY_DECL => (node_field_text(source, node, SW_NAME), Vec::new()),
            SW_PROTOCOL_PROPERTY_DECL => (Self::pattern_bound_identifier(source, node), Vec::new()),
            SW_TYPEALIAS_DECL => (
                node_field_text(source, node, SW_NAME),
                vec![("typealias".to_string(), "true".to_string())],
            ),
            _ => return Vec::new(),
        };
        vec![MemberConstant {
            name,
            visibility: Self::visibility_modifier(source, node),
            properties,
        }]
    }

    fn member_constant_call_body<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        // #100: a computed `property_declaration` scans its accessor body for
        // calls (keyed by the property's QN in `emit_member_constant`). The
        // getter/setter live under the `computed_value` field (a
        // `computed_property`); a stored property with observers carries a
        // `willset_didset_block` child. A STORED property (no accessor) has
        // neither → `None` → nothing scanned, closing the asymmetry with
        // `subscript` (whose `computed_property` body is already scanned via
        // `function_body_kinds`). Typealiases and protocol property requirements
        // are not `property_declaration`s, so they scan nothing.
        if node.kind() != SW_PROPERTY_DECL {
            return None;
        }
        if let Some(computed) = node.child_by_field_name(SW_COMPUTED_VALUE) {
            return Some(computed);
        }
        let mut cursor = node.walk();
        let observers = node
            .children(&mut cursor)
            .find(|c| c.kind() == SW_WILLSET_DIDSET);
        observers
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        Self::callee_tail(source, call_node)
    }

    fn call_entry(
        &self,
        source: &str,
        call_node: Node,
        caller_qn: &str,
        callee: &str,
        seq: u64,
    ) -> CallEntry {
        // `callee` is the tail `call_callee` accepted; it is both the display
        // name, the `callee_name` property, and the `Calls` target (Swift keeps
        // no qualifier). QN keys the call site by position + `#seq`.
        let _ = source;
        let line = call_node.start_position().row + 1;
        let col = call_node.start_position().column + 1;
        CallEntry {
            name: callee.to_string(),
            qualified_name: format!("{caller_qn}::call@{line}:{col}#{seq}"),
            visibility: "internal".to_string(),
            properties: vec![("callee_name".to_string(), callee.to_string())],
            start_line: call_node.start_position().row as u64 + 1,
            end_line: call_node.end_position().row as u64 + 1,
            ref_kind: "Calls",
            ref_to: callee.to_string(),
        }
    }

    fn imports_of(
        &self,
        source: &str,
        _spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        // `import Foundation`, `import struct Foo.Bar` — strip the leading
        // `import` keyword, keep the remainder verbatim as both the display name
        // and the import path/edge target. One statement is one import.
        let text = node_text(source, import_stmt);
        let cleaned = text.trim().trim_start_matches("import").trim().to_string();
        if cleaned.is_empty() {
            return Vec::new();
        }
        vec![ImportEntry {
            display_name: cleaned.clone(),
            qualified_name: qual(scope, &format!("import:{cleaned}")),
            ref_to: cleaned.clone(),
            properties: vec![("path".to_string(), cleaned)],
            visibility: "internal".to_string(),
            start_line: import_stmt.start_position().row as u64 + 1,
            end_line: import_stmt.end_position().row as u64 + 1,
        }]
    }
}

static SWIFT_CONVENTIONS: SwiftConventions = SwiftConventions;

/// The Swift language spec row. All node-kind strings and field names:
/// alex-pinkus/tree-sitter-swift 0.7.3 node-types.json (validated by the spec
/// guard). Swift routes `init`/`deinit`/`subscript` through `function_node_kinds`
/// (named + marked by the conventions), maps the umbrella `class_declaration` via
/// `refine_class_label`, models `enum_entry` as `Variant`s and
/// `property_declaration`/`typealias_declaration` as `member_constant_kinds`. It
/// uses the named `body` field for class/function/init/deinit bodies and
/// `function_body_kinds: ["computed_property"]` only for the field-less
/// `subscript_declaration`. `type_field: "return_type"` is a real Swift field
/// kept for the guard; Swift never reads it (its type-spec/field/value-decl
/// slices are empty).
pub(crate) static SWIFT_SPEC: LangSpec = LangSpec {
    language: Language::Swift,
    skip_node_kinds: &[],
    function_node_kinds: &[
        "function_declaration",
        "protocol_function_declaration",
        "init_declaration",
        "deinit_declaration",
        "subscript_declaration",
    ],
    method_node_kinds: &[],
    class_node_kinds: &["class_declaration"],
    interface_node_kinds: &["protocol_declaration"],
    enum_node_kinds: &[],
    variant_node_kinds: &["enum_entry"],
    member_constant_kinds: &[
        "property_declaration",
        "protocol_property_declaration",
        "typealias_declaration",
    ],
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
    class_body_kinds: &[],
    function_body_kinds: &["computed_property"],
    value_decl_node_kinds: &[],
    value_spec_node_kinds: &[],
    value_name_kind: "simple_identifier",
    variable_declarator_kind: None,
    import_node_kinds: &["import_declaration"],
    import_spec_kinds: &[],
    call_node_kinds: &["call_expression"],
    name_field: "name",
    body_field: Some("body"),
    type_field: "return_type",
    receiver_field: None,
    import_path_field: None,
    extends_field: None,
    value_name_field: None,
    value_type_field: None,
    ts_language: || tree_sitter_swift::LANGUAGE.into(),
    embedded: &[],
    conventions: &SWIFT_CONVENTIONS,
    c_family: None,
    cpp_family: None,
    objc_family: None,
};
