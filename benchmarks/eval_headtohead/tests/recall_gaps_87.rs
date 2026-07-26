//! Regression tests for issue #87 — the recall gaps the #64 head-to-head eval
//! falsified (H4). Each test drives the real index → resolve → cluster pipeline
//! over the committed, content-hashed corpus (the same bytes the published
//! numbers came from) and asserts the specific graph fact the eval row needs,
//! so a re-run cannot silently regress back to losing the row to the Grep
//! baseline.
//!
//! Self-contained (the bench crate exposes no lib target): it re-creates the
//! `corpus::index_language` steps against `CARGO_MANIFEST_DIR/corpus`.

use ai_architect_mcp::clustering::{self, get_impact, get_processes};
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, DependencyScope};
use ai_architect_mcp::resolver;
use std::path::Path;
use tempfile::TempDir;

/// Index one language's corpus through the exact steps the MCP tools (and the
/// GRAPH eval condition) run. The [`TempDir`] is returned so the on-disk graph
/// outlives the queries.
fn index(lang: &str) -> (GraphStore, TempDir) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join(lang);
    let tmp = tempfile::Builder::new()
        .prefix("recall_gaps_87_")
        .tempdir()
        .expect("tempdir");
    let graph_path = tmp.path().join("graph");
    indexer::index_codebase_with_language(&src, &graph_path, None, DependencyScope::None)
        .expect("index corpus");
    let store = GraphStore::open_or_create(&graph_path).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    clustering::cluster_graph(&store, 1.0).expect("cluster");
    (store, tmp)
}

/// Gap 1 (go-D3): "What are the program entry points?" The Go corpus entry is
/// `func Main` in main.go; the graph must expose it as a `main`-kind process, or
/// the D3 row returns nothing (recall 0.0 — the original falsification).
#[test]
fn gap1_go_main_is_a_main_kind_entry_point() {
    let (store, _tmp) = index("go");
    let procs = get_processes(&store).expect("get_processes");
    let main_entries: Vec<&str> = procs
        .iter()
        .filter(|p| p.entry_kind == "main")
        .map(|p| p.entry_point.as_str())
        .collect();
    assert!(
        main_entries.iter().any(|ep| ep.starts_with("main.go::")),
        "issue #87 gap 1: Go `func Main` in main.go must be a main-kind entry \
         point (D3); got main-kind entries {main_entries:?}"
    );
}

/// Every other corpus language already passed D3; pin that the Go fix did not
/// disturb them (their lowercase `main` still resolves to a `main`-kind entry).
#[test]
fn gap1_other_languages_keep_their_main_entry_point() {
    for lang in ["python", "rust", "typescript"] {
        let (store, _tmp) = index(lang);
        let procs = get_processes(&store).expect("get_processes");
        assert!(
            procs.iter().any(|p| p.entry_kind == "main"),
            "issue #87 gap 1: {lang} must still expose a main-kind entry point"
        );
    }
}

/// Gap 3 (rs-D2): "Which functions call `process_order`?" The Rust corpus has
/// two callers — gateway.rs (direct call) and worker.rs (passes it by value to
/// `.map`). The higher-order caller was missing (recall 0.5); both must appear.
#[test]
fn gap3_rust_higher_order_caller_is_captured() {
    let (store, _tmp) = index("rust");
    let impact = get_impact(&store, "core.rs::process_order").expect("get_impact");
    let caller_files: Vec<String> = impact
        .callers
        .iter()
        .map(|c| {
            c.qualified_name
                .split("::")
                .next()
                .unwrap_or(&c.qualified_name)
                .to_string()
        })
        .collect();
    assert!(
        caller_files.iter().any(|f| f == "gateway.rs"),
        "issue #87 gap 3: gateway.rs (direct call) must remain a caller; got {caller_files:?}"
    );
    assert!(
        caller_files.iter().any(|f| f == "worker.rs"),
        "issue #87 gap 3: worker.rs, which passes `process_order` by value to \
         `.map`, must be captured as a caller; got {caller_files:?}"
    );
}
