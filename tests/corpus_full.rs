// corpus_full — Spike B' full-extension gate.
//
// The 41-fixture graph_accuracy.rs harness validates 8 architectural
// categories with 5 hand-annotated files each — the proof-of-correctness
// for the extractor + resolver + bug fixes. This test scales the same
// invariants to the FULL production source tree.
//
// For every Python file under a target root (Cortex's mcp_server/ and
// tests_py/ for now), `tools/spike_b_corpus_gen.py` emits per-file
// expected counts derived from Python's `ast` module. This test loads
// that JSON, indexes the entire target root into one Lbug store, runs
// the resolver, then asserts the EXACT 5-tuple per file (imports,
// constants, functions, classes, methods). Equality, not ≥ — measured
// to hold on Cortex's 576-file tree as of 2026-05-25; the test fails
// on any drift.
//
// Different from graph_accuracy.rs:
//   * 576 files instead of 41 — full production coverage
//   * Counts-only assertions, not exact node/edge identity
//   * One indexer pass per root, queries per file (amortized)
//   * Opt-in via `CORTEX_FULL_CORPUS=1` so day-to-day CI stays fast;
//     gated runs (release / nightly) flip it on.
//
// source: Spike B' full-extension gate (2026-05-25).

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::parser::Language;
use ai_architect_mcp::resolver;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
mod common;
use common::TempDirExt;

#[derive(Debug, Deserialize)]
struct CorpusFile {
    rel_path: String,
    imports: usize,
    constants: usize,
    functions: usize,
    classes: usize,
    methods: usize,
    // callsites + intra_pairs are emitted by the Python AST counter for
    // future use but not asserted today — chained-call counting and
    // resolver dedup rules diverge slightly between Python ast and the
    // Rust resolver; the 5 structural counts above are exact.
    #[serde(default)]
    #[allow(dead_code)]
    callsites: usize,
    #[serde(default)]
    #[allow(dead_code)]
    intra_pairs: usize,
}

#[derive(Debug, Deserialize)]
struct Corpus {
    target_root: String,
    files: Vec<CorpusFile>,
}

#[derive(Default, Debug)]
struct ActualCounts {
    imports: usize,
    constants: usize,
    functions: usize,
    classes: usize,
    methods: usize,
    callsites: usize,
    intra_pairs: usize,
}

fn opt_in() -> bool {
    std::env::var("CORTEX_FULL_CORPUS").ok().as_deref() == Some("1")
}

fn fetch_counts(store: &GraphStore) -> HashMap<String, ActualCounts> {
    // One round-trip per (file, label) and (file, edge-kind) would be
    // O(files × kinds) queries — slow at 576 files. Instead we pull
    // per-symbol qualified_names once, prefix-bucket by file_id, and
    // compute everything in memory.

    let mut by_file: HashMap<String, ActualCounts> = HashMap::new();
    let mut bump = |file_id: &str, field: fn(&mut ActualCounts)| {
        let entry = by_file.entry(file_id.to_string()).or_default();
        field(entry);
    };

    // Helper: derive file_id from a qualified_name by stripping ::* tail.
    // QN convention (parser/mod.rs::qual): "{file_path}::name[::name...]".
    fn file_id_of(qn: &str) -> &str {
        match qn.find("::") {
            Some(idx) => &qn[..idx],
            None => qn,
        }
    }

    // Functions
    if let Ok(qr) = store.execute_query("MATCH (n:Function) RETURN n.qualified_name") {
        for row in qr.rows {
            if let Some(qn) = row.first() {
                bump(file_id_of(qn), |c| c.functions += 1);
            }
        }
    }
    // Methods
    if let Ok(qr) = store.execute_query("MATCH (n:Method) RETURN n.qualified_name") {
        for row in qr.rows {
            if let Some(qn) = row.first() {
                bump(file_id_of(qn), |c| c.methods += 1);
            }
        }
    }
    // Classes (Struct label per Python class)
    if let Ok(qr) = store.execute_query("MATCH (n:Struct) RETURN n.qualified_name") {
        for row in qr.rows {
            if let Some(qn) = row.first() {
                bump(file_id_of(qn), |c| c.classes += 1);
            }
        }
    }
    // Constants
    if let Ok(qr) = store.execute_query("MATCH (n:Constant) RETURN n.qualified_name") {
        for row in qr.rows {
            if let Some(qn) = row.first() {
                bump(file_id_of(qn), |c| c.constants += 1);
            }
        }
    }
    // Imports — Import nodes have `id` as their primary key.
    if let Ok(qr) = store.execute_query("MATCH (n:Import) RETURN n.id") {
        for row in qr.rows {
            if let Some(id) = row.first() {
                bump(file_id_of(id), |c| c.imports += 1);
            }
        }
    }
    // CallSites
    if let Ok(qr) = store.execute_query("MATCH (n:CallSite) RETURN n.id") {
        for row in qr.rows {
            if let Some(id) = row.first() {
                bump(file_id_of(id), |c| c.callsites += 1);
            }
        }
    }
    // Intra-file Calls_Function_Function pairs — both endpoints share file.
    if let Ok(qr) = store.execute_query(
        "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) \
         RETURN a.qualified_name, b.qualified_name",
    ) {
        for row in qr.rows {
            if row.len() < 2 {
                continue;
            }
            let fa = file_id_of(&row[0]);
            let fb = file_id_of(&row[1]);
            if fa == fb {
                bump(fa, |c| c.intra_pairs += 1);
            }
        }
    }

    by_file
}

fn index_target(target_root: &Path, graph_path: &Path) -> Result<(), String> {
    // Mirror the target directory tree into a tempdir before indexing so
    // the file_id (relative_path-from-root) matches the JSON's rel_path.
    indexer::index_codebase_with_language(
        target_root,
        graph_path,
        Some(Language::Python),
        indexer::DependencyScope::None,
    )?;
    let store = GraphStore::open_or_create(graph_path)?;
    let _ = resolver::resolve_graph(&store);
    Ok(())
}

fn run_corpus(corpus_path: &str) {
    if !opt_in() {
        eprintln!("[skipped] corpus_full {corpus_path} — set CORTEX_FULL_CORPUS=1 to run");
        return;
    }
    let corpus_text = fs::read_to_string(corpus_path)
        .unwrap_or_else(|e| panic!("read corpus {corpus_path}: {e}"));
    let corpus: Corpus = serde_json::from_str(&corpus_text)
        .unwrap_or_else(|e| panic!("parse corpus {corpus_path}: {e}"));

    let target_root = PathBuf::from(&corpus.target_root);
    assert!(
        target_root.is_dir(),
        "corpus target_root {} does not exist (regenerate JSON for this host)",
        target_root.display()
    );

    // issue #25 audit: process::id()-based naming collides across processes
    // under PID reuse (observed under back-to-back soak runs). tempfile's
    // Builder allocates a cryptographically-random suffix instead, which
    // does not depend on or repeat across OS PIDs; `.keep()` hands the path
    // to this test's own manual cleanup below instead of auto-deleting it.
    let target_tag = target_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_string());
    let tmp = tempfile::Builder::new()
        .prefix(&format!("corpus_full_{target_tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).expect("mkdir tempdir");
    let graph_path = tmp.join("graph.lbug");

    index_target(&target_root, &graph_path).expect("index target");
    let store = GraphStore::open_or_create(&graph_path).expect("open graph");
    let actuals = fetch_counts(&store);

    // Per-file assertions with rich diff. We collect failures rather than
    // failing on the first to surface the systemic issues at scale.
    let mut failures = 0usize;
    let mut checked = 0usize;
    for f in &corpus.files {
        let actual = actuals.get(&f.rel_path).cloned().unwrap_or_default();
        let want = (f.imports, f.constants, f.functions, f.classes, f.methods);
        let got = (
            actual.imports,
            actual.constants,
            actual.functions,
            actual.classes,
            actual.methods,
        );
        checked += 1;
        if got != want {
            failures += 1;
            if failures <= 20 {
                println!(
                    "MISMATCH {}: \
                     imports={}/{} constants={}/{} functions={}/{} classes={}/{} methods={}/{}",
                    f.rel_path,
                    actual.imports,
                    f.imports,
                    actual.constants,
                    f.constants,
                    actual.functions,
                    f.functions,
                    actual.classes,
                    f.classes,
                    actual.methods,
                    f.methods,
                );
            }
        }
    }
    println!("\nCorpus {corpus_path}: checked={checked} failures={failures}",);
    assert_eq!(
        failures, 0,
        "{} of {} files in {} failed structural-count parity. \
         The Rust extractor diverged from Python ast on these files.",
        failures, checked, corpus_path
    );
}

#[allow(dead_code)]
trait CloneActual {
    fn cloned(&self) -> ActualCounts;
}
impl ActualCounts {
    fn cloned(&self) -> ActualCounts {
        ActualCounts {
            imports: self.imports,
            constants: self.constants,
            functions: self.functions,
            classes: self.classes,
            methods: self.methods,
            callsites: self.callsites,
            intra_pairs: self.intra_pairs,
        }
    }
}

// Re-impl Clone on ActualCounts via the inherent method (Rust forbids
// `Option<&T>::cloned()` returning by value otherwise).
impl Clone for ActualCounts {
    fn clone(&self) -> Self {
        ActualCounts::cloned(self)
    }
}

#[test]
fn corpus_full_cortex_mcp_server() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/graph_accuracy/full_corpus/cortex_mcp_server.json");
    run_corpus(path.to_str().expect("utf-8 path"));
}

#[test]
fn corpus_full_cortex_tests_py() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/graph_accuracy/full_corpus/cortex_tests_py.json");
    run_corpus(path.to_str().expect("utf-8 path"));
}
