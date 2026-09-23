// callsite_target_rows_335 — issue #335: the per-site target tables
// `Calls_CallSite_Function` / `_Method` / `_StdlibSymbol` were declared in
// 50101f8 ("CallSite → callee — emitted by resolver when the callee
// resolves") and never written, so "what does the call at file:line resolve
// to?" had no answer in the graph although the site carried
// `is_resolved = true`. These tests pin one row per resolved site, to the
// same target and with the same provenance as the definition-level edge,
// none for an unresolved site, no duplicate on a second resolve, and no
// leak of the rows into the symbol-level `called_by` / `callers` readers.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{clustering, indexer, resolver, search};
use std::fs;
mod common;
use common::TempDirExt;

/// Line numbers below refer to this text (1-based, as `CallSite.line`).
const FIXTURE: &str = "pub struct Counter {
    n: u32,
}

impl Counter {
    pub fn bump(&mut self) {
        self.n += 1;
    }

    pub fn bump_twice(&mut self) {
        self.bump();
        self.bump();
    }
}

pub fn helper() -> u32 {
    7
}

pub fn entry() -> u32 {
    let x = helper();
    println!(\"x = {}\", x);
    not_defined_anywhere();
    x
}
";

const PER_SITE_TABLES: [&str; 3] = [
    "Calls_CallSite_Function",
    "Calls_CallSite_Method",
    "Calls_CallSite_StdlibSymbol",
];

/// Indexes the fixture and runs one `resolve_graph`. The tempdir guard is
/// returned so the graph outlives the queries.
fn index_and_resolve() -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("callsite_rows_335_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture/src");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::write(src.join("lib.rs"), FIXTURE).expect("write fixture");
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&tmp.path().join("fixture"), &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    (store, tmp)
}

/// Every row of one per-site table as
/// (callsite line, target id, resolution_method, confidence), sorted.
fn per_site_rows(store: &GraphStore, table: &str) -> Vec<(String, String, String, String)> {
    let qr = store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:{table}]->(t) \
             RETURN cs.line, t.id, r.resolution_method, r.confidence"
        ))
        .unwrap_or_else(|e| panic!("query {table}: {e}"));
    let mut rows: Vec<_> = qr
        .rows
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone(), r[2].clone(), r[3].clone()))
        .collect();
    rows.sort();
    rows
}

/// The definition-level edge `caller -[table]-> target` as
/// (resolution_method, confidence); panics unless exactly one exists.
fn definition_edge(
    store: &GraphStore,
    table: &str,
    caller: &str,
    target: &str,
) -> (String, String) {
    let qr = store
        .execute_query(&format!(
            "MATCH (a)-[r:{table}]->(b) WHERE a.id = '{caller}' AND b.id = '{target}' \
             RETURN r.resolution_method, r.confidence"
        ))
        .unwrap_or_else(|e| panic!("query {table}: {e}"));
    assert_eq!(qr.rows.len(), 1, "{table} {caller} -> {target}");
    (qr.rows[0][0].clone(), qr.rows[0][1].clone())
}

fn row_counts(store: &GraphStore) -> Vec<usize> {
    PER_SITE_TABLES
        .iter()
        .map(|t| per_site_rows(store, t).len())
        .collect()
}

#[test]
fn a_resolved_call_site_had_no_per_site_target_row() {
    let (store, _tmp) = index_and_resolve();

    let (method, conf) = definition_edge(
        &store,
        "Calls_Function_Function",
        "src/lib.rs::entry",
        "src/lib.rs::helper",
    );
    assert_eq!(
        per_site_rows(&store, "Calls_CallSite_Function"),
        vec![("21".into(), "src/lib.rs::helper".into(), method, conf)],
        "the `helper()` call at line 21 must have exactly one per-site row, \
         carrying the definition-level edge's provenance"
    );

    // Two sites, one definition-level edge: the per-site table keeps both.
    let (method, conf) = definition_edge(
        &store,
        "Calls_Method_Method",
        "src/lib.rs::Counter::bump_twice",
        "src/lib.rs::Counter::bump",
    );
    let bump = "src/lib.rs::Counter::bump".to_string();
    assert_eq!(
        per_site_rows(&store, "Calls_CallSite_Method"),
        vec![
            ("11".into(), bump.clone(), method.clone(), conf.clone()),
            ("12".into(), bump, method, conf),
        ]
    );
}

#[test]
fn an_unresolved_call_site_gets_no_per_site_row() {
    let (store, _tmp) = index_and_resolve();
    for table in PER_SITE_TABLES {
        assert!(
            !per_site_rows(&store, table).iter().any(|r| r.0 == "23"),
            "`not_defined_anywhere()` (line 23) resolved to nothing, so {table} \
             must hold no row for it"
        );
    }
}

#[test]
fn a_macro_call_site_gets_one_stdlib_row_per_expansion_target() {
    let (store, _tmp) = index_and_resolve();
    let rows = per_site_rows(&store, "Calls_CallSite_StdlibSymbol");
    assert!(
        !rows.is_empty(),
        "println! at line 22 expands to stdlib calls"
    );
    for (line, target, method, conf) in &rows {
        assert_eq!(line, "22");
        let def = definition_edge(
            &store,
            "Calls_Function_StdlibSymbol",
            "src/lib.rs::entry",
            target,
        );
        assert_eq!((method.clone(), conf.clone()), def);
    }
}

#[test]
fn a_second_resolve_duplicates_no_per_site_row() {
    let (store, _tmp) = index_and_resolve();
    let first = row_counts(&store);
    let res = resolver::resolve_graph(&store).expect("second resolve");
    assert_eq!(row_counts(&store), first, "per-site rows are idempotent");
    // `total_edges` counts resolved references, not rows: the per-site rows
    // must not enter it (3 calls + 2 println! expansion targets).
    assert_eq!(res.total_edges, 5);
}

#[test]
fn per_site_rows_leak_into_no_symbol_level_caller_list() {
    let (store, _tmp) = index_and_resolve();
    let ctx = search::get_context(&store, "src/lib.rs::helper").expect("context");
    let called_by: Vec<_> = ctx
        .called_by
        .iter()
        .map(|s| s.qualified_name.as_str())
        .collect();
    assert_eq!(called_by, vec!["src/lib.rs::entry"]);

    let impact = clustering::get_impact(&store, "src/lib.rs::helper").expect("impact");
    let callers: Vec<_> = impact.callers.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(callers, vec!["src/lib.rs::entry"]);
}
