// no_naive_cypher_escape — anti-reintroduction guard for the Cypher
// injection vulnerability fixed in issue #16.
//
// ROOT CAUSE (issue #16): the manual escaping idiom `.replace('\'',
// "\\'")` for building a Cypher string literal is exploitable — a `\'`
// payload turns into `\\'`, an escaped backslash followed by an
// UNescaped closing quote, which closes the literal early and lets the
// remainder of the attacker-controlled string execute as live Cypher
// (e.g. `DETACH DELETE n`). The safe, tested replacement is
// `graph_store::cypher_str` (escapes `\` before `'`, source: Neo4j
// Cypher Manual §"Literals"; see graph_store.rs's module-level comment
// and `test_cypher_str_escape_rules` / `test_cypher_injection_rejected`).
//
// This is the SECOND reintroduction of the naive pattern after a
// documented fix (git_diff.rs's "M1 fix" comment already called out the
// vulnerability by name). A code-review-only defense already failed
// twice; this test makes the constraint mechanical so a third
// reintroduction fails `cargo test` (already run on every push/PR by
// .github/workflows/ci.yml's "Run full test suite" step) instead of
// waiting for a security audit to notice.
//
// Mechanism: scan every `.rs` file under `src/` for the literal source
// text `.replace('\'', "\\'")` (any whitespace variant), fixed-string
// match (not a Cypher-aware parser — see the allowlist note below).
//
// Allowlist: `src/graph_store.rs` is exempt because it OWNS the pattern
// as documentation — `cypher_str`'s module comment and its test module
// describe the naive pattern in prose/asserts to explain why the safe
// helper escapes in the order it does. No other file may contain the
// pattern in any form (code or comment) — a comment mentioning it
// elsewhere is itself a sign the site needs the historical-context
// treatment graph_store.rs already carries, not a proliferation of
// copies. `verdict.rs` / `verdict_tests.rs` document issue #13's
// earlier, already-converted fix; keeping the allowlist to exactly one
// file makes the guard trivial to reason about and keeps history out of
// new files.

use std::fs;
use std::path::{Path, PathBuf};

/// Files allowed to mention the pattern (as documentation of the historical
/// vulnerability, not as a live call). Relative to `src/`.
///
/// `graph_store.rs` was split into the `graph_store/` directory module (§4.1
/// file-size cap); the `cypher_str` documentation of the naive pattern lives in
/// that module's own files (`mod.rs`/`tests.rs`). The allowlist tracks that
/// relocation — the exempt owner is still the single graph_store module.
const ALLOWLIST: &[&str] = &["graph_store/mod.rs", "graph_store/tests.rs"];

/// Builds the exact source text of the forbidden naive-escape idiom,
/// independent of the surrounding call (`x.replace(...)`,
/// `qualified_name.replace(...)`, etc. — the receiver varies, this suffix
/// does not). Built from named parts rather than a single escaped string
/// literal so the pattern the guard searches for is legible instead of a
/// wall of backslashes that could itself hide the very bug being guarded
/// against.
fn forbidden_pattern() -> String {
    let quote = '\'';
    let backslash = '\\';
    format!("replace({quote}{backslash}{quote}{quote}, \"{backslash}{backslash}{quote}\")")
}

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn test_no_naive_cypher_replace_escape_outside_allowlist() {
    let root = src_root();
    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "guard found zero .rs files under {root:?} — path resolution is broken, \
         not a clean codebase"
    );

    let pattern = forbidden_pattern();
    let mut violations = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&root).unwrap_or(path);
        let is_allowlisted = ALLOWLIST.iter().any(|allowed| rel == Path::new(allowed));
        if is_allowlisted {
            continue;
        }
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // non-UTF8 file — not Rust source, skip
        };
        for (line_no, line) in content.lines().enumerate() {
            if line.contains(&pattern) {
                violations.push(format!(
                    "{}:{}: {}",
                    rel.display(),
                    line_no + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "naive Cypher-literal escape `{pattern}` reintroduced outside the \
         graph_store.rs allowlist — use graph_store::cypher_str instead \
         (source: issue #16 RCA, this is the SECOND reintroduction after \
         git_diff.rs's documented M1 fix):\n{}",
        violations.join("\n")
    );
}
