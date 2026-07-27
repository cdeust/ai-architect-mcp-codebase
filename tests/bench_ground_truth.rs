// bench_ground_truth — keeps the SELF-REFERENTIAL benchmark corpus honest
// (issue #132).
//
// `benches/corpora/rust-self/` scores retrieval against this crate's own `src/`,
// so every qualified name in its ground truth names a symbol in this repository.
// When a symbol is renamed or its file deleted — which the ADR-0055 migration
// series did repeatedly, deleting `src/parser/<lang>/` one language at a time —
// the label does not fail: `scoring::score_exact_match` simply scores 0, and the
// benchmark silently under-reports its own accuracy. Two such labels
// (`parse_python_file` from phase 2, `parse_typescript_file` from phase 7) sat
// stale for five phases before anyone noticed.
//
// This test converts that silent zero into a loud failure. It is deliberately
// STATIC (no index, no MCP server, no release binary): it resolves each
// `path/to/file.rs::Symbol` against the source tree and asserts the file exists
// and mentions the symbol. That is enough to catch a deleted file or a renamed
// symbol, it runs in the root crate's `cargo test` (which CI runs — the harness
// crate's own tests would not gate), and it costs milliseconds.
//
// Scope note: only the self-referential corpus can be checked this way. A corpus
// pointing at a vendored third-party tree has no such invariant, so those are
// skipped by construction (this test names `rust-self` explicitly rather than
// globbing every corpus).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Repo root, derived from this test's own location.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every string under a `qualified_name` or `qn` key, anywhere in the document.
/// Both spellings appear: `expected.qualified_name` for the search/exact-match
/// labels, and `qn` inside the clustering label's node list.
fn collect_qualified_names(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if matches!(key.as_str(), "qualified_name" | "qn") {
                    if let Some(s) = child.as_str() {
                        out.insert(s.to_string());
                    }
                }
                collect_qualified_names(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_qualified_names(item, out);
            }
        }
        _ => {}
    }
}

/// Every source path a label's Cypher query pins as a literal: the `f.path =
/// '<path>'` filters, and the file half of a `.qualified_name = '<path>::<sym>'`
/// filter. These are the THIRD way a label names a file (beside `qualified_name`
/// and `qn`), and they rot the same way: `prd_validator.rs` became
/// `prd_validator/mod.rs` and three labels silently matched nothing.
fn collect_cypher_paths(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "query" {
                    if let Some(q) = child.as_str() {
                        for literal in single_quoted(q) {
                            // `path::Symbol` → the file half; a bare path → itself.
                            let path = literal.split("::").next().unwrap_or(literal);
                            if path.ends_with(".rs") {
                                out.insert(path.to_string());
                            }
                        }
                    }
                }
                collect_cypher_paths(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_cypher_paths(item, out);
            }
        }
        _ => {}
    }
}

/// The single-quoted literals in a Cypher query string.
fn single_quoted(query: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = query;
    while let Some(open) = rest.find('\'') {
        let after = &rest[open + 1..];
        match after.find('\'') {
            Some(close) => {
                out.push(&after[..close]);
                rest = &after[close + 1..];
            }
            None => break,
        }
    }
    out
}

/// Splits `parser/spec/walkers/mod.rs::parse_with_spec` into its file path and
/// symbol name. `None` for a name with no `::` separator (not a file-scoped
/// symbol, so there is nothing to resolve).
fn split_qn(qn: &str) -> Option<(&str, &str)> {
    let (path, symbol) = qn.split_once("::")?;
    if path.is_empty() || symbol.is_empty() {
        return None;
    }
    Some((path, symbol))
}

/// Whether `haystack` mentions `symbol` as a whole word — the check that a
/// substring match (`parse_file` inside `parse_typescript_file`) cannot satisfy.
fn mentions_symbol(haystack: &str, symbol: &str) -> bool {
    haystack.match_indices(symbol).any(|(idx, _)| {
        let before_ok = idx == 0
            || !haystack[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after = idx + symbol.len();
        let after_ok = after >= haystack.len()
            || !haystack[after..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        before_ok && after_ok
    })
}

#[test]
fn rust_self_ground_truth_names_only_symbols_that_exist() {
    let root = repo_root();
    let gt_path = root.join("benches/corpora/rust-self/ground_truth.json");
    let text = std::fs::read_to_string(&gt_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", gt_path.display()));
    let doc: Value = serde_json::from_str(&text).expect("ground_truth.json is valid JSON");

    let mut names = BTreeSet::new();
    collect_qualified_names(&doc, &mut names);
    assert!(
        !names.is_empty(),
        "no qualified names found in {} — this test would pass vacuously",
        gt_path.display()
    );

    let src = root.join("src");
    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for qn in &names {
        let Some((rel_path, symbol)) = split_qn(qn) else {
            continue;
        };
        // The corpus indexes `src/`, so a label path is relative to it.
        let file: &Path = &src.join(rel_path);
        if !file.exists() {
            failures.push(format!(
                "{qn}: file src/{rel_path} does not exist (symbol was moved or deleted; \
                 the label scores 0 forever instead of failing)"
            ));
            continue;
        }
        let contents = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        if !mentions_symbol(&contents, symbol) {
            failures.push(format!(
                "{qn}: src/{rel_path} exists but does not mention `{symbol}` \
                 (symbol was renamed; the label scores 0 forever instead of failing)"
            ));
        }
        checked += 1;
    }

    // The third naming channel: source paths embedded in Cypher label queries.
    let mut cypher_paths = BTreeSet::new();
    collect_cypher_paths(&doc, &mut cypher_paths);
    assert!(
        !cypher_paths.is_empty(),
        "no Cypher source paths found in {} — that half of the check would be vacuous",
        gt_path.display()
    );
    for rel_path in &cypher_paths {
        if !src.join(rel_path).exists() {
            failures.push(format!(
                "cypher literal '{rel_path}': file src/{rel_path} does not exist (the \
                 query matches nothing and the label scores 0 forever instead of failing)"
            ));
        }
        checked += 1;
    }

    assert!(
        checked > 0,
        "no file-scoped qualified names were resolved; the check asserted nothing"
    );
    assert!(
        failures.is_empty(),
        "benches/corpora/rust-self/ground_truth.json references {} symbol(s) that no \
         longer exist. A stale label is scored 0 by the harness rather than failing, so \
         it silently depresses the reported benchmark numbers (issue #132). Repoint or \
         remove each one:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The guard above must be able to FAIL. Without this, a bug in `split_qn` or
/// `mentions_symbol` that made everything pass would go unnoticed — the exact
/// vacuous-check failure mode this repo has paid for before.
#[test]
fn the_ground_truth_guard_detects_a_stale_name() {
    let root = repo_root();
    // A deleted file: `src/parser/typescript/` was removed by #60 phase 7.
    let (path, symbol) = split_qn("parser/typescript/mod.rs::parse_typescript_file")
        .expect("a well-formed qualified name splits");
    assert_eq!(path, "parser/typescript/mod.rs");
    assert_eq!(symbol, "parse_typescript_file");
    assert!(
        !root.join("src").join(path).exists(),
        "sanity: the pre-migration TypeScript walker must be gone"
    );
    // A renamed symbol inside a file that DOES exist.
    let existing = root.join("src/parser/mod.rs");
    let contents = std::fs::read_to_string(&existing).expect("read parser/mod.rs");
    assert!(
        mentions_symbol(&contents, "parse_file"),
        "sanity: parse_file must be mentioned in parser/mod.rs"
    );
    assert!(
        !mentions_symbol(&contents, "parse_typescript_file"),
        "sanity: the deleted entry point must NOT be mentioned any more"
    );
    // Whole-word matching: a substring must not count as a mention.
    assert!(!mentions_symbol("fn parse_file_inner() {}", "parse_file"));
}
