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
             imports, or a symbol kind without a line range)"
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
        analysis.epistemic_reasons == vec!["script.py: 2 changed line(s) lie in no indexed symbol (top-level code, imports, or a symbol kind without a line range)".to_string()],
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
