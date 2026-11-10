// detect_changes_methods_446 — issue #446: `detect_changes` reported a modified
// method as its class (TypeScript, Python), a modified Rust `impl` method as
// nothing, and labelled a zero-symbol answer for a non-empty diff `exact`.
// Real indexer, no LSP. Paths are relative to the indexed root (`File.path`).

use ai_architect_mcp::git_diff::{analyze_diff, DiffAnalysis};
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use std::fs;
mod common;
use common::TempDirExt;

const TS: &str = "export class Shape {\n\
    \x20\x20area(): number {\n\
    \x20\x20\x20\x20return 1;\n\
    \x20\x20}\n\
    \x20\x20perimeter(): number {\n\
    \x20\x20\x20\x20return 2;\n\
    \x20\x20}\n\
    }\n";

const PY: &str = "class Model:\n\
    \x20\x20\x20\x20def save(self):\n\
    \x20\x20\x20\x20\x20\x20\x20\x20return 1\n\
    \n\
    \x20\x20\x20\x20def load(self):\n\
    \x20\x20\x20\x20\x20\x20\x20\x20return 2\n";

const RS_TYPES: &str = "pub struct Engine {\n\
    \x20\x20\x20\x20pub power: u32,\n\
    }\n";

const RS_IMPLS: &str = "use crate::types::Engine;\n\
    \n\
    impl Engine {\n\
    \x20\x20\x20\x20pub fn start(&self) -> u32 {\n\
    \x20\x20\x20\x20\x20\x20\x20\x20self.power\n\
    \x20\x20\x20\x20}\n\
    }\n";

const SCRIPT: &str = "import os\n\ndef run():\n    return os.sep\n";

const RS_LIB: &str = "pub mod impls;\npub mod types;\n";

fn indexed() -> (GraphStore, common::TestTempDir) {
    let root = tempfile::Builder::new()
        .prefix("detect_changes_446_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = root.join("src");
    for (rel, body) in [
        ("shapes.ts", TS),
        ("models.py", PY),
        ("lib.rs", RS_LIB),
        ("types.rs", RS_TYPES),
        ("impls.rs", RS_IMPLS),
        ("notes.xyz", "one\ntwo\n"),
        ("script.py", SCRIPT),
        (
            "geo.cpp",
            "namespace geo {\nint area() {\n  return 1;\n}\n}\n",
        ),
        (
            "inline.rs",
            "mod inner {\n    pub fn deep() -> u32 {\n        1\n    }\n}\n",
        ),
    ] {
        fs::create_dir_all(&src).expect("create src");
        fs::write(src.join(rel), body).expect("write fixture");
    }
    let graph_dir = root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    (store, root)
}

/// A one-line modification of `path` at `line`.
fn diff_at(path: &str, line: u64) -> String {
    format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n\
         @@ -{line},1 +{line},1 @@\n-old\n+new\n"
    )
}

fn changed(analysis: &DiffAnalysis) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = analysis
        .symbols_affected
        .iter()
        .map(|s| (s.label.clone(), s.name.clone()))
        .collect();
    out.sort();
    out
}

#[test]
fn modified_nested_symbols_are_reported_as_themselves_not_their_container() {
    let (store, _root) = indexed();
    let cases = [
        // `return 1;` inside `area`
        ("shapes.ts", 3, "Method", "area"),
        // `return 1` inside `save`
        ("models.py", 3, "Method", "save"),
        // `self.power` inside the impl method `start`, whose type is defined
        // in another file
        ("impls.rs", 5, "Method", "start"),
        // a function inside a C++ namespace (a `Struct` whose `Defines` edge
        // the persister drops) and inside a Rust inline `mod`
        ("geo.cpp", 3, "Function", "area"),
        ("inline.rs", 3, "Function", "deep"),
    ];
    for (path, line, label, name) in cases {
        let label_name = (label.to_string(), name.to_string());
        let analysis = analyze_diff(&store, &diff_at(path, line)).expect("analyze_diff");
        assert_eq!(changed(&analysis), vec![label_name], "{path}:{line}");
        assert!(
            !analysis
                .epistemic_reasons
                .iter()
                .any(|r| r.starts_with(path)),
            "{path}: every changed line lies in a symbol: {:?}",
            analysis.epistemic_reasons
        );
        // No caller reaches these methods, so a fully covered diff stays exact.
        assert_eq!(analysis.epistemic, "exact", "{path}");
    }
}

#[test]
fn a_changed_line_outside_every_symbol_makes_the_answer_a_lower_bound() {
    let (store, _root) = indexed();
    // Line 1 is the `use`, line 5 lies in `start`.
    let diff = format!("{}@@ -5,1 +5,1 @@\n-old\n+new\n", diff_at("impls.rs", 1));
    let analysis = analyze_diff(&store, &diff).expect("analyze_diff");
    assert_eq!(
        changed(&analysis),
        vec![("Method".to_string(), "start".to_string())]
    );
    assert_eq!(analysis.symbols_affected[0].lines_changed, 1);
    assert_eq!(analysis.epistemic, "lower-bound");
    assert_eq!(
        analysis.epistemic_reasons,
        vec![
            "impls.rs: 1 changed line(s) lie in no indexed symbol (top-level code, \
             imports, a removal between symbols, or a declaration the graph does \
             not index)"
                .to_string()
        ]
    );
}

#[test]
fn an_added_file_with_top_level_code_is_a_lower_bound() {
    let (store, _root) = indexed();
    let diff = "diff --git a/script.py b/script.py\n--- /dev/null\n+++ b/script.py\n\
                @@ -0,0 +1,4 @@\n+import os\n+\n+def run():\n+    return os.sep\n";
    let analysis = analyze_diff(&store, diff).expect("analyze_diff");
    assert_eq!(
        changed(&analysis),
        vec![("Function".to_string(), "run".to_string())]
    );
    assert_eq!(analysis.epistemic, "lower-bound");
    assert!(
        analysis.epistemic_reasons == vec!["script.py: 1 changed line(s) lie in no indexed symbol (top-level code, imports, a removal between symbols, or a declaration the graph does \
             not index)".to_string()],
        "{:?}",
        analysis.epistemic_reasons
    );
}

#[test]
fn a_diff_touching_only_unsupported_files_is_never_exact() {
    let (store, _root) = indexed();
    let analysis = analyze_diff(&store, &diff_at("notes.xyz", 1)).expect("analyze_diff");
    assert!(analysis.symbols_affected.is_empty());
    assert_eq!(analysis.epistemic, "lower-bound");
    assert_eq!(
        analysis.epistemic_reasons.len(),
        1,
        "{:?}",
        analysis.epistemic_reasons
    );
    assert!(
        analysis.epistemic_reasons[0].starts_with("notes.xyz: "),
        "{:?}",
        analysis.epistemic_reasons
    );
}

#[test]
fn one_incomplete_file_in_a_mixed_diff_gives_exactly_one_reason() {
    let (store, _root) = indexed();
    let diff = format!("{}{}", diff_at("shapes.ts", 3), diff_at("notes.xyz", 1));
    let analysis = analyze_diff(&store, &diff).expect("analyze_diff");
    assert_eq!(
        changed(&analysis),
        vec![("Method".to_string(), "area".to_string())]
    );
    assert_eq!(analysis.epistemic, "lower-bound");
    assert_eq!(
        analysis.epistemic_reasons.len(),
        1,
        "{:?}",
        analysis.epistemic_reasons
    );
    assert!(analysis.epistemic_reasons[0].starts_with("notes.xyz: "));
    assert_eq!(analysis.coverage_gap_count, 1);
}

/// Claude review of PR #482: a removal used to be credited to the line after
/// it. Removing a method between `area` and `perimeter` changes their class,
/// not `perimeter`; removing a top-level item just above `impl Engine` changes
/// no symbol.
#[test]
fn a_removal_goes_to_the_symbol_spanning_it_not_to_the_next_one() {
    let (store, _root) = indexed();
    let in_class = "diff --git a/shapes.ts b/shapes.ts\n--- a/shapes.ts\n+++ b/shapes.ts\n\
                    @@ -4,5 +4,2 @@\n   }\n-  gone(): number {\n-    return 3;\n-  }\n   perimeter(): number {\n";
    let analysis = analyze_diff(&store, in_class).expect("analyze_diff");
    assert_eq!(
        changed(&analysis),
        vec![("Struct".to_string(), "Shape".to_string())]
    );
    assert_eq!(analysis.coverage_gap_count, 0);

    let top_level = "diff --git a/impls.rs b/impls.rs\n--- a/impls.rs\n+++ b/impls.rs\n\
                     @@ -2,3 +2,2 @@\n \n-pub const GONE: u32 = 1;\n impl Engine {\n";
    let analysis = analyze_diff(&store, top_level).expect("analyze_diff");
    assert!(
        analysis.symbols_affected.is_empty(),
        "{:?}",
        changed(&analysis)
    );
    assert_eq!(analysis.epistemic, "lower-bound");
    assert_eq!(analysis.coverage_gap_count, 1);
}

#[test]
fn a_deleted_file_reports_every_symbol_it_held() {
    let (store, _root) = indexed();
    let mut diff =
        "diff --git a/shapes.ts b/shapes.ts\n--- a/shapes.ts\n+++ /dev/null\n@@ -1,8 +0,0 @@\n"
            .to_string();
    for line in TS.lines() {
        diff.push_str(&format!("-{line}\n"));
    }
    let analysis = analyze_diff(&store, &diff).expect("analyze_diff");
    assert_eq!(
        changed(&analysis),
        vec![
            ("Method".to_string(), "area".to_string()),
            ("Method".to_string(), "perimeter".to_string()),
            ("Struct".to_string(), "Shape".to_string()),
        ]
    );
    assert!(analysis
        .symbols_affected
        .iter()
        .all(|s| s.change_type == "deleted"));
    assert_eq!(
        analysis.coverage_gap_count, 0,
        "{:?}",
        analysis.epistemic_reasons
    );
    assert_eq!(analysis.epistemic, "exact");
}

/// Claude review of PR #482: on an indexed file, a binary section and a hunk
/// that overruns its header are reported, not answered `exact`.
#[test]
fn binary_and_unreadable_sections_of_an_indexed_file_are_lower_bounds() {
    let (store, _root) = indexed();
    let binary = "diff --git a/shapes.ts b/shapes.ts\nindex 1..2 100644\n\
                  Binary files a/shapes.ts and b/shapes.ts differ\n";
    let overrun = "diff --git a/shapes.ts b/shapes.ts\n--- a/shapes.ts\n+++ b/shapes.ts\n\
                   @@ -3 +3 @@\n-old\n+new\n+extra\n";
    for (diff, reason) in [
        (binary, "shapes.ts: binary content"),
        (
            overrun,
            "shapes.ts: part of this file's diff could not be read",
        ),
    ] {
        let analysis = analyze_diff(&store, diff).expect("analyze_diff");
        assert_eq!(analysis.epistemic, "lower-bound", "{reason}");
        assert!(
            analysis
                .epistemic_reasons
                .iter()
                .any(|r| r.starts_with(reason)),
            "{reason}: {:?}",
            analysis.epistemic_reasons
        );
    }
}

/// Codex security review of PR #482: `get_impact` runs for at most
/// `IMPACT_BUDGET` (256) distinct symbols per diff. Every changed symbol is
/// still reported, and the answer past the budget is `lower-bound`.
#[test]
fn impact_expansion_is_bounded_for_many_symbols_on_one_line() {
    let root = tempfile::Builder::new()
        .prefix("detect_changes_446_budget_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");
    let line: String = (0..300)
        .map(|n| format!("pub const C{n}: u32 = {n}; "))
        .collect();
    fs::write(src.join("lib.rs"), format!("{line}\n")).expect("write fixture");
    let graph_dir = root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");

    let analysis = analyze_diff(&store, &diff_at("lib.rs", 1)).expect("analyze_diff");
    assert_eq!(analysis.symbols_affected.len(), 300);
    assert_eq!(analysis.epistemic, "lower-bound");
    assert!(
        analysis
            .epistemic_reasons
            .iter()
            .any(|r| r.starts_with("44 changed symbols past the first 256")),
        "{:?}",
        analysis.epistemic_reasons
    );
}

/// Codex security review of PR #482: `analyze_git_diff` keeps rename
/// detection, so a pure rename is one hunk-less section (not the whole file
/// deleted and added) and is reported as a `lower-bound` reason.
#[test]
fn git_diff_of_a_pure_rename_is_one_small_lower_bound_section() {
    let (store, root) = indexed();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("create repo");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&repo)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    let body: String = (0..2000).map(|n| format!("line {n}\n")).collect();
    fs::write(repo.join("old.txt"), &body).expect("write");
    git(&["add", "old.txt"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["mv", "old.txt", "new.txt"]);
    git(&["commit", "-q", "-m", "rename"]);

    let analysis = ai_architect_mcp::git_diff::analyze_git_diff(&store, &repo, "HEAD~1", "HEAD")
        .expect("analyze_git_diff");
    assert_eq!(analysis.files_changed, 1);
    assert_eq!(analysis.epistemic, "lower-bound");
    assert!(
        analysis
            .epistemic_reasons
            .iter()
            .any(|r| r.starts_with("new.txt: renamed or copied")),
        "{:?}",
        analysis.epistemic_reasons
    );
}

/// Codex security review of PR #482: the `a.rs::` id prefix also matches the
/// symbols of an indexed `a.rs::b.rs`. They are not credited to `a.rs`, so a
/// one-line decoy there cannot hide the real, wider function.
#[test]
fn symbols_of_a_nested_path_file_are_not_credited_to_its_prefix() {
    let root = tempfile::Builder::new()
        .prefix("detect_changes_446_nested_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(
        src.join("a.rs"),
        "pub fn wide() -> u32 {\n    let x = 1;\n    let y = 2;\n    x + y\n}\n",
    )
    .expect("write a.rs");
    fs::write(src.join("a.rs::b.rs"), "\n\npub fn decoy() -> u32 { 0 }\n")
        .expect("write a.rs::b.rs");
    let graph_dir = root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");

    let analysis = analyze_diff(&store, &diff_at("a.rs", 3)).expect("analyze_diff");
    assert_eq!(
        changed(&analysis),
        vec![("Function".to_string(), "wide".to_string())],
        "{:?}",
        analysis.epistemic_reasons
    );
}
