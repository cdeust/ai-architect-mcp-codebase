// parser — Multi-language tree-sitter parser module.
//
// Dispatches to language-specific parsers (Rust, Python, TypeScript).
// All parsers produce the same ParseResult/ExtractedNode/ExtractedRef types.
// The indexer calls `parse_file(source, file_path, language)` and gets a
// uniform result regardless of language.

pub mod c;
pub mod cpp;
pub mod objc;
pub mod rust;
pub mod typescript;

// Table-driven extraction (ADR-0055). Migrated languages route through the
// generic spec walkers; the rest stay on the hand-written modules above.
mod spec;

// source: H2 fix — per-file tree-sitter parse timeout. 5 seconds is ~100x
// the typical parse time for a 1 MB source file on an M-series Mac (measured
// ~50 ms in practice) and matches the default suggested in the tree-sitter
// CLI (`--time-limit 5`). Parser::parse returns None when this is exceeded.
pub(crate) const PARSE_TIMEOUT_MICROS: u64 = 5_000_000;

use std::time::{Duration, Instant};

use tree_sitter::{Node, ParseOptions, ParseState, Parser, Tree};

mod language;
pub use language::Language;

// ---------------------------------------------------------------------------
// Output labels — match graph_store NODE_* constants by value, not by import.
// ---------------------------------------------------------------------------

pub const LABEL_FUNCTION: &str = "Function";
pub const LABEL_METHOD: &str = "Method";
pub const LABEL_STRUCT: &str = "Struct";
pub const LABEL_ENUM: &str = "Enum";
pub const LABEL_VARIANT: &str = "Variant";
pub const LABEL_TRAIT: &str = "Trait";
pub const LABEL_FIELD: &str = "Field";
pub const LABEL_CONSTANT: &str = "Constant";
pub const LABEL_TYPE_ALIAS: &str = "TypeAlias";
pub const LABEL_IMPORT: &str = "Import";
pub const LABEL_MODULE: &str = "Module";
pub const LABEL_CALL_SITE: &str = "CallSite";

// ---------------------------------------------------------------------------
// Public types — shared contract for all language parsers
// ---------------------------------------------------------------------------

pub struct ParseResult {
    pub nodes: Vec<ExtractedNode>,
    pub refs: Vec<ExtractedRef>,
    // source: stages/stage-3.md §6.1 (parse_errors on the parser-port output)
    // and §10.5 ("Parse errors per file ... dropping it hides broken parses
    // behind clean node counts"). Count of tree-sitter ERROR/MISSING nodes in
    // the parsed tree — tree-sitter recovers from bad syntax into ERROR/MISSING
    // nodes rather than failing, so a non-fatal parse can still be degraded.
    // Cross-ref: GitNexus safe-parse.ts detects `root.hasError || root.isMissing`.
    pub parse_errors: u32,
    // 1-based inclusive (start_line, end_line) spans of the ERROR/MISSING regions
    // in the parse tree — the coverage-honesty signal (issue #57): constructs
    // inside these lines may be missing from the graph, so an agent should grep
    // them. Empty when the parse is clean. Capped (see `collect_error_ranges`) so
    // a pathologically broken file cannot bloat the coverage sidecar.
    // source: DeusData/codebase-memory-mcp index_coverage table — `parse_partial`
    // rows store 1-based line ranges as their `detail`.
    pub error_ranges: Vec<(u32, u32)>,
}

pub struct ExtractedNode {
    pub label: String,
    pub name: String,
    pub qualified_name: String,
    pub start_line: u64,
    pub end_line: u64,
    pub visibility: String,
    pub properties: Vec<(String, String)>,
}

pub struct ExtractedRef {
    pub kind: String,
    pub from_qualified_name: String,
    pub to_qualified_name: String,
}

// ---------------------------------------------------------------------------
// Unified entry point
// ---------------------------------------------------------------------------

/// Parses a source file and extracts typed symbols and relationships.
/// Dispatches to the appropriate language-specific parser.
pub fn parse_file(source: &str, file_path: &str, lang: Language) -> Result<ParseResult, String> {
    match lang {
        Language::Rust => rust::parse_rust_file(source, file_path),
        Language::TypeScript => typescript::parse_typescript_file(source, file_path),
        Language::ObjC => objc::parse_objc_file(source, file_path),
        Language::C => c::parse_c_file(source, file_path),
        Language::Cpp => cpp::parse_cpp_file(source, file_path),
        // Migrated to the table-driven spec walkers (ADR-0055).
        // Go: phase 1 (#85). Python: phase 2 (#89). Java: phase 3 (#91).
        // Kotlin: phase 4 (#95). Swift: phase 5 (#60).
        Language::Go => spec::parse_with_spec(&spec::GO_SPEC, source, file_path),
        Language::Python => spec::parse_with_spec(&spec::PYTHON_SPEC, source, file_path),
        Language::Java => spec::parse_with_spec(&spec::JAVA_SPEC, source, file_path),
        Language::Kotlin => spec::parse_with_spec(&spec::KOTLIN_SPEC, source, file_path),
        Language::Swift => spec::parse_with_spec(&spec::SWIFT_SPEC, source, file_path),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers used by all language parsers
// ---------------------------------------------------------------------------

/// Extracts the full text of a node from source.
pub(crate) fn node_text(source: &str, node: Node) -> String {
    source[node.byte_range()].to_string()
}

/// Extracts text from a named field of a node. Returns empty string if absent.
pub(crate) fn node_field_text(source: &str, node: Node, field: &str) -> String {
    node.child_by_field_name(field)
        .map(|n| node_text(source, n))
        .unwrap_or_default()
}

/// Builds a qualified name with `::` separator (normalized form).
pub(crate) fn qual(scope: &str, name: &str) -> String {
    format!("{scope}::{name}")
}

/// Counts tree-sitter ERROR and MISSING nodes anywhere in the tree.
///
/// tree-sitter never throws on malformed input — it recovers into ERROR
/// (unparseable span) and MISSING (inserted-to-recover) nodes and returns a
/// tree anyway. A clean `Ok(ParseResult)` with zero symbols can therefore mean
/// either "empty file" or "the parser is keyed to the wrong grammar and every
/// construct became an ERROR node". This count is the quality signal that
/// distinguishes the two. source: stages/stage-3.md §10.5.
pub(crate) fn count_parse_errors(root: Node) -> u32 {
    let mut errors: u32 = 0;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            errors += 1;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    errors
}

/// Maximum number of error ranges recorded per file. A file keyed to the wrong
/// grammar can produce thousands of ERROR nodes; recording every one would bloat
/// the coverage sidecar for no added signal — the point is "this file is gappy;
/// grep it", which a bounded, merged list conveys. Beyond the cap the ranges are
/// truncated (the count on the File node still reflects the true total).
// source: heuristic — 64 disjoint bad regions is far more than any real file has
// while a wrong-grammar file trips the cap and is flagged wholesale anyway.
const MAX_ERROR_RANGES: usize = 64;

/// Collects the 1-based inclusive line spans of ERROR/MISSING nodes, merging
/// adjacent/overlapping spans so a run of sibling error tokens on the same lines
/// becomes one range. Returns at most `MAX_ERROR_RANGES` ranges (sorted by start
/// line). Empty for a clean parse. This is the coverage-honesty detail (#57):
/// the graph may be missing constructs inside these lines.
pub(crate) fn collect_error_ranges(root: Node) -> Vec<(u32, u32)> {
    let mut spans: Vec<(u32, u32)> = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            let start = node.start_position().row as u32 + 1;
            let end = node.end_position().row as u32 + 1;
            spans.push((start, end.max(start)));
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    if spans.is_empty() {
        return spans;
    }
    spans.sort_unstable();
    // Merge overlapping/adjacent spans (gap of 0 lines) into contiguous ranges.
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 + 1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged.truncate(MAX_ERROR_RANGES);
    merged
}

/// Parses `source` under the shared per-file timeout guard and returns the tree.
///
/// tree-sitter 0.25 deprecated `Parser::set_timeout_micros`; the supported
/// replacement is a progress callback supplied through `ParseOptions`. Per the
/// tree-sitter C API (`api.h`: "Parsing was cancelled due to the progress
/// callback returning true"), the callback returns `true` to CANCEL — so we
/// return `true` once the wall-clock deadline of `PARSE_TIMEOUT_MICROS` passes.
/// `parse_with_options` takes an input-reader closure; for an in-memory `&str`
/// it hands tree-sitter the remaining byte slice from each requested offset.
/// Returns `Err` on timeout-cancel, source rejection, or a `None` tree.
/// source: tree-sitter v0.25 api.h TSParseOptions / ts_parser_parse_with_options.
pub(crate) fn parse_with_timeout(parser: &mut Parser, source: &str) -> Result<Tree, String> {
    let deadline = Instant::now() + Duration::from_micros(PARSE_TIMEOUT_MICROS);
    let mut past_deadline = |_state: &ParseState| Instant::now() >= deadline;
    let options = ParseOptions::new().progress_callback(&mut past_deadline);
    let bytes = source.as_bytes();
    parser
        .parse_with_options(
            &mut |offset, _pos| bytes.get(offset..).unwrap_or(&[]),
            None,
            Some(options),
        )
        .ok_or_else(|| {
            "parse_timeout_or_none: tree-sitter returned None \
             (parse cancelled, timeout exceeded, or source rejected)"
                .to_string()
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("java"), Some(Language::Java));
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
        // JS family is parsed with the TypeScript grammar (JS ⊂ TS).
        assert_eq!(Language::from_extension("js"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("jsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("mjs"), Some(Language::TypeScript));
    }

    #[test]
    fn test_language_from_str() {
        assert_eq!(Language::from_str_opt("rust"), Some(Language::Rust));
        assert_eq!(Language::from_str_opt("python"), Some(Language::Python));
        assert_eq!(
            Language::from_str_opt("typescript"),
            Some(Language::TypeScript)
        );
        assert_eq!(Language::from_str_opt("java"), Some(Language::Java));
        assert_eq!(Language::from_str_opt("go"), Some(Language::Go));
        // "auto" is not a concrete language — detection happens by extension.
        assert_eq!(Language::from_str_opt("auto"), None);
    }

    #[test]
    fn test_qual() {
        assert_eq!(qual("src/main.rs", "foo"), "src/main.rs::foo");
    }
}
