//! End-to-end cursor pagination over a real fixture graph, one test per
//! bounded-read tool shape. Each test asserts the cursor's correctness
//! contract (see response_budget::BoundedPage docs):
//!   - page-through reconstruction: union of pages == unpaged full set,
//!     in order, no duplicates, no gaps;
//!   - next_offset absent on the final page;
//!   - offset beyond end → empty page, no next_offset;
//!   - stable order: two identical calls return the same order, so the
//!     cursor is safe.
use super::*;
use std::fs;

// A fixture where one symbol (`helpers::sanitize`) is called from several
// call sites across modules, giving get_impact a multi-element `callers`
// list and query_graph/search several rows to page over.
const F_MAIN: &str = r#"
use crate::svc;
fn main() { let _ = svc::run_a(); let _ = svc::run_b(); }
fn driver() { let _ = svc::run_c(); }
"#;
const F_SVC: &str = r#"
use crate::helpers;
pub fn run_a() -> String { helpers::sanitize("a") }
pub fn run_b() -> String { helpers::sanitize("b") }
pub fn run_c() -> String { helpers::sanitize("c") }
pub fn run_d() -> String { helpers::sanitize("d") }
"#;
const F_HELPERS: &str = r#"
pub fn sanitize(input: &str) -> String { input.trim().to_string() }
"#;

/// Builds an indexed + resolved + clustered fixture graph. Returns the
/// tempdir guard (keep it alive for the caller's whole test — dropping it
/// early deletes the graph out from under the test) alongside the graph dir.
fn build_fixture(tag: &str) -> (crate::test_support::TestTempDir, std::path::PathBuf) {
    use crate::test_support::TempDirExt;
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix(&format!("pagination_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);
    let src = tmp_root.join("fixture/src");
    fs::create_dir_all(&src).expect("create fixture src");
    fs::write(src.join("main.rs"), F_MAIN).unwrap();
    fs::write(src.join("svc.rs"), F_SVC).unwrap();
    fs::write(src.join("helpers.rs"), F_HELPERS).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = graph_store::GraphStore::open_or_create(&graph_dir).unwrap();
    resolver::resolve_graph(&store).expect("resolve");
    clustering::cluster_graph(&store, 1.0).expect("cluster");
    // Drop the store handle so the read-path cache opens its own; the
    // embedded DB is single-writer and tests share a process.
    drop(store);
    (tmp_root, graph_dir)
}

/// Drives a tool's cursor to exhaustion: repeatedly calls `call(offset)`,
/// extracting the primary list with `items` and the cursor with `next`.
/// Returns the concatenation of every page's items and the final-page flag
/// sequence. Asserts the cursor strictly advances (terminates).
fn page_through<F>(mut call: F) -> Vec<Value>
where
    F: FnMut(u64) -> Value,
{
    let mut all = Vec::new();
    let mut offset: u64 = 0;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 10_000, "cursor failed to terminate");
        let resp = call(offset);
        let items = resp["__items"].as_array().cloned().unwrap_or_default();
        all.extend(items.iter().cloned());
        match resp.get("next_offset").and_then(|v| v.as_u64()) {
            Some(n) => {
                assert!(n > offset, "cursor must advance: {n} <= {offset}");
                offset = n;
            }
            None => break,
        }
    }
    all
}

// -- get_processes -----------------------------------------------------

#[test]
fn get_processes_pages_through_everything() {
    let (_guard, graph) = build_fixture("procs");
    let gp = graph.to_str().unwrap().to_string();

    let call = |offset: u64| -> Value {
        let mut r =
            do_get_processes(&json!({"graph_path": gp, "offset": offset})).expect("get_processes");
        r["__items"] = r["processes"].clone();
        r
    };

    // Full unpaged set (offset 0 returns all for this small fixture).
    let full = call(0);
    let total = full["total_count"].as_u64().unwrap() as usize;
    assert!(total > 0, "fixture must yield processes");

    // Page-through union == full set, no dupes, no gaps.
    let paged = page_through(call);
    assert_eq!(paged.len(), total, "paged count must equal total");
    assert_eq!(paged, full["processes"].as_array().cloned().unwrap());

    // Offset beyond end → empty, no cursor.
    let beyond =
        do_get_processes(&json!({"graph_path": gp, "offset": total as u64 + 100})).unwrap();
    assert_eq!(beyond["processes"].as_array().unwrap().len(), 0);
    assert!(beyond.get("next_offset").is_none());
    assert!(!beyond["truncated"].as_bool().unwrap());

    // Stable order: two identical calls return identical order.
    let a = do_get_processes(&json!({"graph_path": gp})).unwrap();
    let b = do_get_processes(&json!({"graph_path": gp})).unwrap();
    assert_eq!(a["processes"], b["processes"], "order must be stable");
}

// -- get_impact (primary list = callers) -------------------------------

#[test]
fn get_impact_pages_callers_through_everything() {
    let (_guard, graph) = build_fixture("impact");
    let gp = graph.to_str().unwrap().to_string();

    // Use the real node id from the graph (qualified_name format varies by
    // resolver), exactly as stage3c_integration looks it up. `sanitize` is
    // called from run_a/run_b/run_c/run_d → >=3 resolved callers.
    let store = graph_store::GraphStore::open_or_create(&graph).unwrap();
    let qr = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'sanitize' RETURN f.id")
        .unwrap();
    assert!(!qr.rows.is_empty(), "sanitize node must exist");
    let target = qr.rows[0][0].clone();
    drop(store);

    let call = |offset: u64| -> Value {
        let mut r = do_get_impact(&json!({
            "graph_path": gp, "qualified_name": target, "offset": offset
        }))
        .expect("get_impact");
        r["__items"] = r["callers"].clone();
        r
    };

    let full = call(0);
    let total = full["callers_total"].as_u64().unwrap() as usize;
    assert!(total >= 3, "sanitize should have >=3 callers, got {total}");
    assert_eq!(full["primary_list"], json!("callers"));
    assert_eq!(full["secondary_lists_paged"], json!(false));

    let paged = page_through(call);
    assert_eq!(paged.len(), total);
    assert_eq!(paged, full["callers"].as_array().cloned().unwrap());

    // Offset beyond end → empty callers, no cursor.
    let beyond = do_get_impact(&json!({
        "graph_path": gp, "qualified_name": target, "offset": total as u64 + 50
    }))
    .unwrap();
    assert_eq!(beyond["callers"].as_array().unwrap().len(), 0);
    assert!(beyond.get("next_offset").is_none());

    // Stable order across identical calls.
    let a = do_get_impact(&json!({"graph_path": gp, "qualified_name": target})).unwrap();
    let b = do_get_impact(&json!({"graph_path": gp, "qualified_name": target})).unwrap();
    assert_eq!(a["callers"], b["callers"], "callers order must be stable");
}

/// fleet-watch#19: get_impact must resolve the caller's qualified_name before
/// its strict-equality blast-radius query, exactly as get_symbol/get_context
/// do. The parser strips the leading path component when building
/// qualified_names, so the README's own `src/main.rs::foo` form matched no node
/// and get_impact returned an empty caller set with `status: ok` — a vacuous,
/// misleading result rather than the real blast radius (or an honest error).
/// Here the same symbol is queried by its stored qualified_name and by a
/// `src/`-prefixed variant the stored form does not carry; both must return the
/// identical, non-empty caller set.
///
/// Adds one `Imports_File_File` edge from `svc.rs` to `helpers.rs` and returns
/// the latter's `File.id`. File-level fan-in is what a File target is FOR, so
/// the fixture states it explicitly rather than depending on which import
/// shape the resolver happens to emit for `use crate::helpers;`.
fn with_file_level_import_edge(graph: &std::path::Path) -> String {
    let store = graph_store::GraphStore::open_or_create(graph).unwrap();
    let file_id = |name: &str| -> String {
        let qr = store
            .execute_query(&format!(
                "MATCH (f:File) WHERE f.name = {} RETURN f.id",
                graph_store::cypher_str(name)
            ))
            .unwrap();
        assert!(!qr.rows.is_empty(), "the fixture must index {name}");
        qr.rows[0][0].clone()
    };
    let helpers = file_id("helpers.rs");
    let svc = file_id("svc.rs");
    store
        .insert_edge(
            "Imports_File_File",
            &svc,
            &helpers,
            &[("confidence", "1.0")],
        )
        .expect("insert file-level import edge");
    helpers
}

#[test]
fn get_impact_on_a_file_target_is_not_symbol_not_found() {
    // Review finding 2 (regression), end to end. `get_impact` answers
    // file-level fan-in for a File target (issue #205), but the resolve gate
    // added in this branch probed only the eight symbol labels — so every
    // File target came back `symbol_not_found` and that whole capability was
    // unreachable through the tool.
    let (_guard, graph) = build_fixture("impact_file_target");
    let gp = graph.to_str().unwrap().to_string();
    let helpers = with_file_level_import_edge(&graph);

    let out = do_get_impact(&json!({"graph_path": gp, "qualified_name": helpers.clone()})).unwrap();
    assert_eq!(
        out["status"],
        json!("ok"),
        "a File target must be answered, not refused: {out}"
    );
    assert_eq!(
        out["qualified_name"],
        json!(helpers),
        "the response must name the File the numbers describe"
    );
    assert_eq!(
        out["importers_total"],
        json!(1),
        "the File target's file-level fan-in must be computed: {out}"
    );

    // An unknown path is still refused — the resolve gate is widened, not removed.
    let missing =
        do_get_impact(&json!({"graph_path": gp, "qualified_name": "no/such/file.rs"})).unwrap();
    assert_eq!(missing["reason"], json!("symbol_not_found"));
}

#[test]
fn get_impact_resolves_src_prefixed_qualified_name() {
    let (_guard, graph) = build_fixture("impact_src_prefix");
    let gp = graph.to_str().unwrap().to_string();

    // The stored qualified_name (parser has already stripped the leading path
    // component), then the README-style `src/`-prefixed variant of it.
    let store = graph_store::GraphStore::open_or_create(&graph).unwrap();
    let qr = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'sanitize' RETURN f.qualified_name")
        .unwrap();
    assert!(!qr.rows.is_empty(), "sanitize node must exist");
    let stored = qr.rows[0][0].clone();
    drop(store);
    let prefixed = format!("src/{stored}");
    assert_ne!(
        prefixed, stored,
        "the prefixed form must differ from the stored one"
    );

    let stored_impact =
        do_get_impact(&json!({"graph_path": gp, "qualified_name": stored})).unwrap();
    let prefixed_impact =
        do_get_impact(&json!({"graph_path": gp, "qualified_name": prefixed})).unwrap();

    let stored_total = stored_impact["callers_total"].as_u64().unwrap();
    assert!(
        stored_total >= 3,
        "stored form must resolve to sanitize's >=3 callers, got {stored_total}"
    );

    // The src/-prefixed form must resolve to the SAME blast radius — not the
    // pre-fix vacuous empty-but-"ok" result.
    assert_eq!(
        prefixed_impact["status"],
        json!("ok"),
        "src/-prefixed form must resolve, not error"
    );
    assert_eq!(
        prefixed_impact["callers_total"], stored_impact["callers_total"],
        "src/-prefixed form must yield the same callers_total as the stored form"
    );
    assert_eq!(
        prefixed_impact["callers"], stored_impact["callers"],
        "resolved callers must be identical for both name forms"
    );
}

// -- search_codebase ---------------------------------------------------

#[test]
fn search_codebase_pages_through_everything() {
    let (_guard, graph) = build_fixture("search");
    let gp = graph.to_str().unwrap().to_string();

    let call = |offset: u64| -> Value {
        let mut r = do_search_codebase(&json!({
            "graph_path": gp, "query": "run", "limit": 50, "offset": offset
        }))
        .expect("search");
        r["__items"] = r["results"].clone();
        r
    };

    let full = call(0);
    let total = full["total_count"].as_u64().unwrap() as usize;
    assert!(total > 0, "search must yield results for 'run'");

    let paged = page_through(call);
    assert_eq!(paged.len(), total);
    assert_eq!(paged, full["results"].as_array().cloned().unwrap());

    // Offset beyond end → empty, no cursor.
    let beyond = do_search_codebase(&json!({
        "graph_path": gp, "query": "run", "limit": 50, "offset": total as u64 + 100
    }))
    .unwrap();
    assert_eq!(beyond["results"].as_array().unwrap().len(), 0);
    assert!(beyond.get("next_offset").is_none());

    // Stable order across identical calls (deterministic score+name sort).
    let a = do_search_codebase(&json!({"graph_path": gp, "query": "run", "limit": 50})).unwrap();
    let b = do_search_codebase(&json!({"graph_path": gp, "query": "run", "limit": 50})).unwrap();
    assert_eq!(a["results"], b["results"], "search order must be stable");
}

// -- query_graph (caller-ordered; ORDER BY makes the cursor safe) -------

#[test]
fn query_graph_pages_through_ordered_query() {
    let (_guard, graph) = build_fixture("query");
    let gp = graph.to_str().unwrap().to_string();
    // ORDER BY makes the row order stable → cursor is safe (order_stable).
    let q = "MATCH (f:Function) RETURN f.qualified_name ORDER BY f.qualified_name";

    let call = |offset: u64| -> Value {
        let mut r = do_query_graph(&json!({
            "graph_path": gp, "query": q, "offset": offset
        }))
        .expect("query_graph");
        r["__items"] = r["rows"].clone();
        r
    };

    let full = call(0);
    assert_eq!(full["order_stable"], json!(true), "ORDER BY → order_stable");
    let total = full["total_count"].as_u64().unwrap() as usize;
    assert!(total > 0, "fixture has functions");

    let paged = page_through(call);
    assert_eq!(paged.len(), total);
    assert_eq!(paged, full["rows"].as_array().cloned().unwrap());

    // Offset beyond end → empty rows, no cursor.
    let beyond = do_query_graph(&json!({
        "graph_path": gp, "query": q, "offset": total as u64 + 100
    }))
    .unwrap();
    assert_eq!(beyond["rows"].as_array().unwrap().len(), 0);
    assert!(beyond.get("next_offset").is_none());

    // A query WITHOUT ORDER BY must report order_stable=false (honest flag).
    let unordered = do_query_graph(&json!({
        "graph_path": gp, "query": "MATCH (f:Function) RETURN f.qualified_name"
    }))
    .unwrap();
    assert_eq!(
        unordered["order_stable"],
        json!(false),
        "no ORDER BY → order_stable must be false"
    );
}
