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

    /// Edge kind for an import ref. Default: `Imports` (Go). Python emits
    /// `Defines` for imports (file-local declaration edges) and overrides.
    fn import_ref_kind(&self) -> &'static str {
        "Imports"
    }
}
