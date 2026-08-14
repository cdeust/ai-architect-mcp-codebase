//! Integration test for issue #92 — cross-language type-usage (`Uses`) edges
//! from return-type annotations and type-construction expressions.
//!
//! Drives the real index → resolve pipeline over tiny inline Go / Rust / TS
//! corpora and asserts the `Uses_<caller>_<Type>` edges the resolver's
//! `resolve_callable_type_uses` emits. This is the main-crate home for the
//! resolver logic the #64 eval's `recall_gaps_87.rs::gap2_*` cover end-to-end
//! (that suite lives in the separate bench crate); keeping a copy here means the
//! resolver phase is exercised — and its mutants killed — by this crate's own
//! test suite.

use std::fs;

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, IndexOptions};
use ai_architect_mcp::resolver;
use tempfile::TempDir;

/// Writes `files` (name → contents) into a temp source dir, indexes and
/// resolves it, and returns the open graph. The [`TempDir`] is returned so the
/// on-disk graph outlives the queries.
fn build_graph(files: &[(&str, &str)]) -> (GraphStore, TempDir) {
    let src = tempfile::Builder::new()
        .prefix("uses92_src_")
        .tempdir()
        .expect("src tempdir");
    for (name, body) in files {
        fs::write(src.path().join(name), body).expect("write source");
    }
    let graph_dir = tempfile::Builder::new()
        .prefix("uses92_graph_")
        .tempdir()
        .expect("graph tempdir");
    let graph_path = graph_dir.path().join("graph");
    indexer::index_codebase_with_language(src.path(), &graph_path, &IndexOptions::default())
        .expect("index");
    let store = GraphStore::open_or_create(&graph_path).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    // Keep both tempdirs alive for the duration of the queries: leak the source
    // dir (small, cleaned at process exit) and return the graph dir handle.
    std::mem::forget(src);
    (store, graph_dir)
}

/// True iff some `Uses_*_*` edge exists whose source id contains `from_substr`
/// and whose target id contains `to_substr`.
fn has_uses_edge(store: &GraphStore, from_substr: &str, to_substr: &str) -> bool {
    for &(rel, _, _) in ai_architect_mcp::graph_store::REL_TABLES {
        if !rel.starts_with("Uses_") {
            continue;
        }
        let q = format!("MATCH (a)-[:{rel}]->(b) RETURN a.id, b.id");
        let Ok(qr) = store.execute_query(&q) else {
            continue;
        };
        for row in &qr.rows {
            if row.len() >= 2 && row[0].contains(from_substr) && row[1].contains(to_substr) {
                return true;
            }
        }
    }
    false
}

#[test]
fn go_return_type_and_construction_produce_uses_edges() {
    let core = "package p\n\
        type OrderConfig struct { Retries int }\n\
        func LoadConfig() OrderConfig { return OrderConfig{Retries: 3} }\n\
        func Warm() { _ = OrderConfig{Retries: 1} }\n";
    let api = "package p\n\
        func ApplyConfig() OrderConfig { return LoadConfig() }\n";
    let (store, _g) = build_graph(&[("core.go", core), ("api.go", api)]);
    // LoadConfig: return type OrderConfig AND constructs OrderConfig.
    assert!(
        has_uses_edge(&store, "core.go::LoadConfig", "OrderConfig"),
        "Go LoadConfig must Use OrderConfig (return type + construction)"
    );
    // ApplyConfig: return type OrderConfig only (same-package, no import) —
    // isolates the return-type resolution path.
    assert!(
        has_uses_edge(&store, "api.go::ApplyConfig", "OrderConfig"),
        "Go ApplyConfig must Use OrderConfig (return type)"
    );
    // Warm: constructs OrderConfig but returns nothing — isolates the
    // type-construction resolution path (kills a mutant that drops it).
    assert!(
        has_uses_edge(&store, "core.go::Warm", "OrderConfig"),
        "Go Warm must Use OrderConfig (construction only, no return)"
    );
}

#[test]
fn rust_return_type_and_construction_produce_uses_edges() {
    let core = "pub struct OrderConfig { pub retries: u32 }\n\
        pub fn load_config() -> OrderConfig { OrderConfig { retries: 3 } }\n";
    let (store, _g) = build_graph(&[("core.rs", core)]);
    assert!(
        has_uses_edge(&store, "core.rs::load_config", "OrderConfig"),
        "Rust load_config must Use OrderConfig (return type + struct literal)"
    );
}

#[test]
fn typescript_return_type_and_construction_produce_uses_edges() {
    let core = "export class OrderConfig {}\n\
        export function loadConfig(): OrderConfig { return new OrderConfig(); }\n";
    let (store, _g) = build_graph(&[("core.ts", core)]);
    assert!(
        has_uses_edge(&store, "core.ts::loadConfig", "OrderConfig"),
        "TS loadConfig must Use OrderConfig (return annotation + new)"
    );
}

#[test]
fn method_return_type_produces_uses_method_edge() {
    // A class METHOD (not a free function) that returns/constructs a type must
    // also produce a `Uses_Method_<Type>` edge — isolates the `Method` arm of the
    // resolver's caller-label loop (kills a mutant that only iterates `Function`).
    let core = "export class OrderConfig {}\n\
        export class Factory {\n  make(): OrderConfig { return new OrderConfig(); }\n}\n";
    let (store, _g) = build_graph(&[("core.ts", core)]);
    assert!(
        has_uses_edge(&store, "Factory::make", "OrderConfig"),
        "TS method Factory.make must Use OrderConfig (return + new)"
    );
}

/// Negative: a function that neither returns nor constructs a nominal type emits
/// no `Uses` edge (guards against the resolver fabricating edges from empty or
/// primitive-only properties — the mutant that drops the `find_type_target`
/// None-guard).
#[test]
fn primitive_only_signature_produces_no_uses_edge() {
    let core = "package p\n\
        type OrderConfig struct { Retries int }\n\
        func Add(a int, b int) int { return a + b }\n";
    let (store, _g) = build_graph(&[("core.go", core)]);
    assert!(
        !has_uses_edge(&store, "core.go::Add", "OrderConfig"),
        "a primitive-only function must not Use OrderConfig"
    );
    // And no Uses edge from Add to anything at all.
    assert!(
        !has_uses_edge(&store, "core.go::Add", ""),
        "a primitive-only function must emit no Uses edge"
    );
}
