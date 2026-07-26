// parser::spec::conventions — the behavioral escape hatch (ADR-0055 §4).
//
// `LangSpec` carries *structure* (which node kinds mean what). This trait
// carries *behavior* that will not reduce to node-kind slices: visibility
// rules, qualified-name shaping (Go's `#seq` dedup suffix, Python's dedup-on-
// collision), call-site shaping (edge kind, QN scheme, callee text), and
// import-statement interpretation. Mirrors the existing `LanguageProvider`
// trait+registry the resolver already trusts.
//
// "Default impl + per-language overrides only where real conventions exist"
// (ADR-0055 §4): the trait provides defaults for the genuinely trivial,
// grammar-independent predicates (a value is always a constant unless a
// language says otherwise; a function carries no extra props; a constant's
// visibility follows the general visibility rule; the import edge kind is
// `Imports`). Everything grammar-specific is a required method a language
// implements. This is the honest split the ADR argues: **data for structure,
// a small trait for behavior**. The size of a language's override is the
// ADR Risk-#1 watch signal ("richer-than-CBM leakage") — measured per
// migration, not hidden.

use tree_sitter::Node;

use super::lang_spec::LangSpec;
use crate::parser::node_field_text;

/// One extracted import: the `Import` node's shape plus its outgoing edge.
/// Produced by `LanguageConventions::imports_of`; consumed by `walk_imports`.
/// One import *statement* can yield several of these (Python
/// `from foo import a, b, c`), so `imports_of` returns a `Vec`.
pub(crate) struct ImportEntry {
    /// Display name (the `Import` node's `name`).
    pub display_name: String,
    /// Fully-qualified name (the `Import` node's primary key).
    pub qualified_name: String,
    /// Edge target of the import ref (`to_qualified_name`). Go: the import
    /// path; Python: the `Import` node's own QN (a file-local `Defines`).
    pub ref_to: String,
    /// Node properties (Go `[("path", …)]`; Python `[path, alias, is_glob]`).
    pub properties: Vec<(String, String)>,
    /// Node visibility.
    pub visibility: String,
    /// 1-based start line of the `Import` node. Go: the import-spec line;
    /// Python: the import-statement line.
    pub start_line: u64,
    /// 1-based end line of the `Import` node.
    pub end_line: u64,
}

/// One extracted call site: the `CallSite` node's shape plus its outgoing edge.
/// Produced by `LanguageConventions::call_entry`; consumed by `walk_calls`.
/// This DTO absorbs every cross-language call divergence — edge kind
/// (Go `Calls` vs Python `Defines`), QN scheme (Go `#seq` vs Python byte
/// span), callee text (Go last segment vs Python full attribute path),
/// visibility, properties, and end-line convention — so the walker stays
/// generic.
pub(crate) struct CallEntry {
    /// The `CallSite` node's display name (the callee).
    pub name: String,
    /// The `CallSite` node's primary key.
    pub qualified_name: String,
    /// Node visibility (Go `public`; Python empty).
    pub visibility: String,
    /// Node properties (Go `[callee_name]`; Python `[callee_name, caller_qn]`).
    pub properties: Vec<(String, String)>,
    /// 1-based start line.
    pub start_line: u64,
    /// 1-based end line (Go: call-node end row; Python: start line).
    pub end_line: u64,
    /// Outgoing ref kind (Go `Calls`; Python `Defines`).
    pub ref_kind: &'static str,
    /// Outgoing ref target (Go: the callee name; Python: this node's QN).
    pub ref_to: String,
}

/// The inheritance a class-like node declares: the node properties recording
/// it and the outgoing edges. Produced by `LanguageConventions::class_inheritance`;
/// consumed by `walk_defs`' class emitter. One DTO absorbs the cross-language
/// divergence: Python has a single `superclasses` list (→ `bases` property +
/// `Extends` refs, `.`-normalized); Java splits `extends` (one superclass →
/// `bases` + `Extends`) from `implements` (interface list → `implements` +
/// `Implements`), and only records a property when the clause is present.
pub(crate) struct ClassInheritance {
    /// Properties to attach to the class node, in emission order.
    pub properties: Vec<(String, String)>,
    /// Outgoing edges `(ref_kind, to_qualified_name)`; `from` is the class QN.
    pub refs: Vec<(&'static str, String)>,
}

/// One extracted member constant: the `Constant` node's shape. Produced by
/// `LanguageConventions::member_constants`; consumed by `walk_defs`' member-
/// constant emitter for `member_constant_kinds` nodes. Two Kotlin shapes route
/// here: an `enum_entry` → `Constant` with an `enum_entry=true` property and
/// implicit `public` visibility (name from the entry's identifier child), and a
/// `property_declaration` (`val`/`var`, class-member or top-level) → `Constant`
/// with modifier-derived visibility and no marker property, matching Java's
/// field-as-`Constant` model — its name read one level below the direct-child
/// identifier scan, from the `variable_declaration` child (issue #93). The
/// emitter attaches the enclosing-scope `Defines` edge.
pub(crate) struct MemberConstant {
    /// The `Constant` node's display name (empty ⇒ the node is skipped).
    pub name: String,
    /// Node visibility.
    pub visibility: String,
    /// Node properties, in emission order.
    pub properties: Vec<(String, String)>,
}

/// Behavioral predicates and shaping for one language. Object-safe and `Sync`
/// so a `&'static dyn LanguageConventions` can live in a `static LangSpec`.
pub(crate) trait LanguageConventions: Sync {
    // --- required: genuinely grammar-specific behavior ---

    /// Visibility string for a declared name (Go: uppercase ⇒ `public`;
    /// Python: leading underscore ⇒ `private`, dunder ⇒ public).
    fn visibility_of(&self, name: &str) -> String;

    /// Receiver type extracted from a method's receiver text, or empty when
    /// the language has no receiver concept. Drives Go method QN scoping;
    /// only called for `method_node_kinds`.
    fn receiver_type(&self, receiver_text: &str) -> String;

    /// Base ("raw") qualified name for a function/method definition, before
    /// the walker's collision dedup. Go: `{scope}::{name}#{seq}` (already
    /// unique, so dedup is a no-op). Python: `{scope}::{name}` (dedup appends
    /// `@line` on collision — the `@property`/`@setter` case).
    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String;

    /// Callee name for a call node, or `None` to drop the call (a
    /// non-identifier callee). This is BOTH the skip decision and the callee
    /// text: Go returns the last dotted segment (`fmt.Println` → `Println`);
    /// Python returns the full function text (`self.foo` → `self.foo`).
    fn call_callee(&self, source: &str, call_node: Node) -> Option<String>;

    /// Shapes a call node (whose callee `call_callee` already accepted) into a
    /// `CallEntry`. `seq` is the per-file counter (Go's `#seq`; unused by
    /// Python, which keys on the byte span).
    fn call_entry(
        &self,
        source: &str,
        call_node: Node,
        caller_qn: &str,
        callee: &str,
        seq: u64,
    ) -> CallEntry;

    /// Interprets one import *statement* node into zero or more `ImportEntry`.
    /// Go descends to `import_spec_kinds`; Python dispatches on the three
    /// import-statement kinds and their `dotted_name`/`aliased_import`/
    /// `wildcard_import` children.
    fn imports_of(
        &self,
        source: &str,
        spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry>;

    // --- defaults: trivial, grammar-independent ---

    /// The name of a function/method-like definition, given its node and the
    /// spec's `name_field`. Default: the `name_field` child's text (Go/Python/
    /// Java/Kotlin — every function-like node names itself through one field).
    /// Swift overrides for `init`/`deinit`/`subscript` declarations, which the
    /// grammar gives no usable `name` field and which the graph names
    /// synthetically (`init`/`deinit`/`subscript`) — the same names the hand-
    /// written walker synthesized.
    fn def_name(&self, source: &str, node: Node, name_field: &str) -> String {
        node_field_text(source, node, name_field)
    }

    /// The outgoing edge kind for an enum member (`variant_node_kinds`).
    /// Default: `HasVariant` (Java `enum_constant`). Swift overrides to `Defines`
    /// (its hand-written walker modelled enum cases as `Defines`-edged variants).
    fn variant_edge_kind(&self) -> &'static str {
        "HasVariant"
    }

    /// The visibility for an enum member node. Default: `public` (Java enum
    /// constants are implicitly public). Swift overrides to `internal` (its
    /// hand-written walker's enum-case default).
    fn variant_visibility(&self) -> String {
        "public".to_string()
    }

    /// Whether a value-declaration name is a constant. Default: yes. Python
    /// overrides (only `UPPER_SNAKE` module assignments are constants).
    fn is_constant_name(&self, _name: &str) -> bool {
        true
    }

    /// Extra properties for a function/method node, before decorators and
    /// receiver are appended. Default: none. Python overrides (`is_async`).
    // mutation note (§12): the `Vec::new()` → `vec![]` mutant here is a proven
    // EQUIVALENT mutant — both construct the same empty `Vec`, so no test can
    // observe a difference. Not a coverage gap.
    fn function_props(&self, _source: &str, _node: Node) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Visibility for a `Constant` node. Default: the general visibility rule
    /// (Go). Python overrides to empty (module constants carry no visibility,
    /// even `_PRIVATE`-looking `UPPER_SNAKE` names).
    fn constant_visibility(&self, name: &str) -> String {
        self.visibility_of(name)
    }

    /// Edge kind for an import ref, given the import STATEMENT node that
    /// produced the entries. Default: `Imports` (Go). Python emits `Defines`
    /// for imports (file-local declaration edges) and overrides unconditionally.
    ///
    /// The node is a parameter because Rust is one language with TWO import edge
    /// kinds: a `use` declaration emits a file-local `Defines` (the imported
    /// name becomes a symbol in this file), while `extern crate` emits an
    /// `Imports` edge to the crate name. That distinction is a property of the
    /// statement, so it can only be decided with the statement in hand.
    fn import_ref_kind(&self, _import_stmt: Node) -> &'static str {
        "Imports"
    }

    /// Extra `CallSite`s a single accepted call node yields BEYOND the one
    /// `call_entry` shapes, appended in order. Default: none — one call node is
    /// one call site for every language but Rust.
    ///
    /// Rust overrides it for the higher-order-argument capture (issue #87): a
    /// function passed *by value* (`queue.iter().map(process_order)`) is a real
    /// reference to that function, but the argument identifier is not itself a
    /// call node, so the DFS would never emit a call site for it and the
    /// resolver would never record the `Calls` edge.
    // mutation note (§12): the `Vec::new()` → `vec![]` mutant here is a proven
    // EQUIVALENT mutant — both construct the same empty `Vec`, so no test can
    // observe a difference (same precedent as `function_props` above). The
    // default is also unreachable for the non-Rust languages in the sense that
    // it can only ever contribute zero entries. Not a coverage gap.
    fn extra_call_entries(
        &self,
        _source: &str,
        _call_node: Node,
        _caller_qn: &str,
    ) -> Vec<CallEntry> {
        Vec::new()
    }

    /// Visibility for a declared node, given both its AST node and its name.
    /// Default: the name-based rule (Go uppercase / Python underscore) — the
    /// name is the only signal. Java overrides to read the node's `modifiers`
    /// child (`public`/`private`/`protected`, default package), which the name
    /// cannot carry.
    fn node_visibility(&self, _source: &str, _node: Node, name: &str) -> String {
        self.visibility_of(name)
    }

    /// Refines the label for a class-like node whose grammar uses ONE node kind
    /// (`class_declaration`) for several concepts, disambiguated by content
    /// rather than by node kind. Default: the label `class_like_label` already
    /// picked from the spec slices (Go/Python/Java, whose grammars use distinct
    /// kinds). Kotlin overrides to inspect the node (`interface` keyword →
    /// `Trait`, `enum` / `enum_class_body` → `Enum`, else `Struct`).
    fn refine_class_label(
        &self,
        _source: &str,
        _node: Node,
        default_label: &'static str,
    ) -> &'static str {
        default_label
    }

    /// Shapes one `member_constant_kinds` node into zero or more
    /// `MemberConstant`s. A single grammar node can bind several names — Kotlin's
    /// destructuring `property_declaration` `val (a, b) = …` yields two — so the
    /// return is a `Vec`: empty skips the node (an empty/malformed name), one
    /// entry is the common case (an `enum_entry` or a single `val`/`var`), many
    /// is a destructuring binding. Default: empty — only languages that populate
    /// `member_constant_kinds` (Kotlin) reach this, so the default is never
    /// called for Go/Python/Java (§9: no test for an unreachable path).
    // mutation note (§12): the `Vec::new()` → `vec![]` mutant here is a proven
    // EQUIVALENT mutant — both construct the same empty `Vec`, so no test can
    // observe a difference (same precedent as `function_props` above). It is also
    // an unreachable path for the migrated set (only Kotlin populates
    // `member_constant_kinds`, and Kotlin overrides this method). Not a coverage
    // gap.
    fn member_constants(&self, _source: &str, _node: Node) -> Vec<MemberConstant> {
        Vec::new()
    }

    /// The accessor body of a `member_constant_kinds` node to scan for calls,
    /// or `None` when the member has no scannable body. Default: `None` — a
    /// member constant is a leaf (Kotlin `val`/`var` and `enum_entry`, whose
    /// initializer the graph does not scan). Swift overrides for a computed
    /// `property_declaration` (issue #100): its getter/setter/observer body
    /// (`computed_property` / `willset_didset_block`) IS scanned, keyed by the
    /// property's own QN, matching the call-scan a `subscript`'s
    /// `computed_property` already receives via `function_body_kinds`. A stored
    /// property (no accessor body) yields `None`, so it scans nothing — the
    /// asymmetry issue #100 closes.
    // mutation note (§12): the default `None` is an unreachable-for-Kotlin path
    // (only Swift populates a computed `property_declaration` body); returning
    // `Some` here for the migrated set is impossible, so the default is not test-
    // observable for Kotlin — EQUIVALENT (§9). Swift's override is exercised by
    // the parity corpus (`magnitude`) and the #100 fidelity test.
    fn member_constant_call_body<'t>(&self, _node: Node<'t>) -> Option<Node<'t>> {
        None
    }

    /// The inheritance a class-like node declares. Default: the single-list
    /// model driven by `spec.extends_field` + `spec.base_node_kinds` (Python),
    /// emitting an always-present `bases` property and one `.`-normalized
    /// `Extends` ref per base. Java overrides to split `extends` from
    /// `implements` and to record a property only when the clause is present.
    fn class_inheritance(&self, source: &str, spec: &LangSpec, node: Node) -> ClassInheritance {
        let bases = super::walkers::collect_bases(spec, source, node);
        let refs = bases
            .iter()
            .map(|b| ("Extends", b.replace('.', "::")))
            .collect();
        ClassInheritance {
            properties: vec![("bases".to_string(), bases.join(","))],
            refs,
        }
    }
}
