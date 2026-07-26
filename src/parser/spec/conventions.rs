// parser::spec::conventions — the behavioral escape hatch (ADR-0055 §4).
//
// `LangSpec` carries *structure* (which node kinds mean what). This trait
// carries *behavior* that will not reduce to node-kind slices: visibility
// rules, qualified-name shaping (Go's `#seq` dedup suffix, receiver scoping),
// call-callee extraction, and import-entry shaping. Mirrors the existing
// `LanguageProvider` trait+registry the resolver already trusts.
//
// "Default impl + per-language overrides only where real conventions exist"
// (ADR-0055 §4): the trait provides defaults for the genuinely trivial,
// grammar-independent predicates (a value is always a constant unless a
// language says otherwise; a function carries no extra props; the import edge
// kind is `Imports`). Everything grammar-specific is a required method a
// language implements. As languages migrate and a behavior proves shared by
// three of them, it is promoted to a default (coding-standards §3.3 — three
// concrete uses before extracting). Seeding speculative defaults now would be
// dead code (§9), so Phase 1 keeps the required set explicit.

use tree_sitter::Node;

use super::lang_spec::LangSpec;

/// One extracted import: the `Import` node's shape plus its outgoing edge.
/// Produced by `LanguageConventions::import_entry`; consumed by `walk_imports`.
pub(crate) struct ImportEntry {
    /// Display name (the `Import` node's `name`).
    pub display_name: String,
    /// Fully-qualified name (the node's primary key).
    pub qualified_name: String,
    /// Edge target of the import ref (`to_qualified_name`).
    pub ref_to: String,
    /// Node properties (e.g. `("path", "net/http")`).
    pub properties: Vec<(String, String)>,
    /// Node visibility.
    pub visibility: String,
}

/// Behavioral predicates and shaping for one language. Object-safe and `Sync`
/// so a `&'static dyn LanguageConventions` can live in a `static LangSpec`.
pub(crate) trait LanguageConventions: Sync {
    // --- required: genuinely grammar-specific behavior ---

    /// Visibility string for a declared name (e.g. Go: uppercase ⇒ `public`).
    fn visibility_of(&self, name: &str) -> String;

    /// Receiver type extracted from a method's receiver text, or empty when
    /// the language has no receiver concept. Drives method QN scoping.
    fn receiver_type(&self, receiver_text: &str) -> String;

    /// Qualified name for a function/method definition. `seq` is a
    /// per-file monotonic counter for languages that disambiguate overloads
    /// by sequence (Go) rather than by source position.
    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String;

    /// Qualified name for a call site. `line`/`col` are 1-based.
    fn call_site_qn(&self, caller: &str, line: usize, col: usize, seq: u64) -> String;

    /// Callee name for a call node, or `None` to drop the call (e.g. a
    /// non-identifier callee). `call_node` is the matched call expression.
    fn call_callee(&self, source: &str, call_node: Node) -> Option<String>;

    /// Shapes one import spec into an `ImportEntry`, or `None` to skip it.
    /// Reads `spec.import_path_field` for the path.
    fn import_entry(
        &self,
        source: &str,
        spec: &LangSpec,
        import_spec_node: Node,
        scope: &str,
    ) -> Option<ImportEntry>;

    // --- defaults: trivial, grammar-independent (consumed by Go) ---

    /// Whether a value-declaration name is a constant. Default: yes. Python
    /// will override (only `UPPER_SNAKE` module assignments are constants).
    fn is_constant_name(&self, _name: &str) -> bool {
        true
    }

    /// Extra properties for a function/free-function node. Default: none.
    /// Python will override (`is_async`, `decorators`).
    // mutation note (§12): the `Vec::new()` → `vec![]` mutant here is a proven
    // EQUIVALENT mutant — both construct the same empty `Vec`, so no test can
    // observe a difference. Not a coverage gap.
    fn function_props(&self, _source: &str, _node: Node) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Edge kind for an import ref. Default: `Imports`. Python emits `Defines`
    /// for imports and will override.
    fn import_ref_kind(&self) -> &'static str {
        "Imports"
    }
}
