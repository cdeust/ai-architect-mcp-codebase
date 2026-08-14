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
use ai_architect_mcp::indexer::{self, IndexOptions};
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
    indexer::index_codebase_with_language(&src, &graph_path, &IndexOptions::default())
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

// ---------------------------------------------------------------------------
// Gap 2 (D4) — issue #92: cross-language type-usage (Uses) edges for
// return-types and type-construction expressions. Before #92 the graph had NO
// return-type / type-construction reference data, so `get_impact(OrderConfig)`
// lost recall to the Grep baseline: go-D4 = 0.0 (nothing), rs-D4 / ts-D4 = 0.5
// (missed core.rs / core.ts). Each test below asserts the D4 ground-truth files
// surface as USERS of `OrderConfig`, so a re-run cannot regress the row.
// ---------------------------------------------------------------------------

/// Collects the distinct file prefixes of a type's D4 dependents, mirroring the
/// eval's `op_impact_users` exactly: reverse `Uses` (users) UNION reverse
/// `Imports` (importers). Go's same-package `OrderConfig` has no importer, so it
/// depends entirely on the new Uses edges; the imported languages combine both.
fn user_files(store: &GraphStore, type_qn: &str) -> Vec<String> {
    let impact = get_impact(store, type_qn).expect("get_impact");
    let mut files: Vec<String> = impact
        .users
        .iter()
        .chain(impact.importers.iter())
        .map(|u| {
            u.qualified_name
                .split("::")
                .next()
                .unwrap_or(&u.qualified_name)
                .to_string()
        })
        .collect();
    files.sort();
    files.dedup();
    files
}

/// go-D4: `LoadConfig` (return type + composite-literal construction) and
/// `ApplyConfig` (return type) both use `OrderConfig`; core.go AND api.go must
/// surface (recall 1.0, up from 0.0 — no import existed for the Imports fallback).
#[test]
fn gap2_go_d4_return_and_construction_uses() {
    let (store, _tmp) = index("go");
    let files = user_files(&store, "core.go::OrderConfig");
    assert!(
        files.iter().any(|f| f == "core.go"),
        "issue #92 go-D4: core.go (return type + `OrderConfig{{}}` construction) \
         must use OrderConfig; got {files:?}"
    );
    assert!(
        files.iter().any(|f| f == "api.go"),
        "issue #92 go-D4: api.go (return type OrderConfig) must use OrderConfig; got {files:?}"
    );
}

/// rs-D4: core.rs (`-> OrderConfig` + `OrderConfig {..}` struct literal) was
/// the missed file (recall 0.5). Both core.rs and api.rs must surface now.
#[test]
fn gap2_rust_d4_return_and_construction_uses() {
    let (store, _tmp) = index("rust");
    let files = user_files(&store, "core.rs::OrderConfig");
    assert!(
        files.iter().any(|f| f == "core.rs"),
        "issue #92 rs-D4: core.rs (return type + struct-literal construction) \
         must use OrderConfig; got {files:?}"
    );
    assert!(
        files.iter().any(|f| f == "api.rs"),
        "issue #92 rs-D4: api.rs (return type OrderConfig) must use OrderConfig; got {files:?}"
    );
}

/// ts-D4: core.ts (`: OrderConfig` return annotation + `new OrderConfig()`) was
/// the missed file (recall 0.5). Both core.ts and api.ts must surface now.
#[test]
fn gap2_typescript_d4_return_and_construction_uses() {
    let (store, _tmp) = index("typescript");
    let files = user_files(&store, "core.ts::OrderConfig");
    assert!(
        files.iter().any(|f| f == "core.ts"),
        "issue #92 ts-D4: core.ts (return annotation + `new OrderConfig()`) \
         must use OrderConfig; got {files:?}"
    );
    assert!(
        files.iter().any(|f| f == "api.ts"),
        "issue #92 ts-D4: api.ts (return annotation OrderConfig) must use OrderConfig; got {files:?}"
    );
}

/// Python already passed D4 (recall 1.0) via the plain-call construction path;
/// pin that #92 did not disturb it (core.py + api.py still use OrderConfig).
#[test]
fn gap2_python_d4_unchanged() {
    let (store, _tmp) = index("python");
    let files = user_files(&store, "core.py::OrderConfig");
    assert!(
        files.iter().any(|f| f == "core.py") && files.iter().any(|f| f == "api.py"),
        "issue #92 py-D4: core.py and api.py must still use OrderConfig; got {files:?}"
    );
}
