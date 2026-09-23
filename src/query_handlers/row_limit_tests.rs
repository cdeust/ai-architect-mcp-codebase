//! Issue #334: the injected row bound must be reported when it cuts a result,
//! and paging must reach every row past it.
//!
//! Before the fix, a query with no LIMIT ran with `LIMIT 500`; a larger result
//! came back as 500 rows with `total_count: 500` and `truncated: false`, and no
//! offset could reach row 501.

use super::*;
use crate::graph_store::{GraphStore, QueryResult, NODE_FUNCTION};

const N: usize = QUERY_GRAPH_ROW_LIMIT;

/// Pages `rows` (already fetched by the engine) as `run_gated_cypher` does.
fn page_of(mut rows: Vec<Vec<String>>, offset: u64, limit_injected: bool) -> Value {
    let row_limit_reached = limit_injected && clip_probe_row(&mut rows, offset);
    let qr = QueryResult {
        columns: vec!["n.id".to_string()],
        rows,
    };
    paged_query_response(
        &qr,
        QueryPageMeta {
            offset,
            elapsed_ms: 0,
            order_stable: true,
            limit_injected,
            row_limit_reached,
        },
    )
}

fn rows(count: usize) -> Vec<Vec<String>> {
    (0..count).map(|i| vec![format!("f{i:04}")]).collect()
}

#[test]
fn an_injected_limit_that_cuts_the_result_reports_truncated() {
    // The engine answered the probe: N + 1 rows came back for LIMIT N + 1.
    let page = page_of(rows(N + 1), 0, true);
    assert_eq!(page["truncated"], json!(true), "{page}");
    assert_eq!(page["returned_count"], json!(N));
    assert_eq!(page["rows"].as_array().map(Vec::len), Some(N));
    assert_eq!(page["row_limit"], json!(N));
    assert_eq!(page["next_offset"], json!(N));
    assert_eq!(page["limit_injected"], json!(true));
}

#[test]
fn a_result_of_exactly_the_row_limit_is_not_truncated() {
    let page = page_of(rows(N), 0, true);
    assert_eq!(page["truncated"], json!(false), "{page}");
    assert_eq!(page["total_count"], json!(N));
    assert_eq!(page["row_limit"], json!(N));
    assert!(page.get("next_offset").is_none(), "{page}");
}

#[test]
fn the_probe_window_follows_the_offset() {
    // Page 2 of a 2N-row result: LIMIT offset + N + 1 returned 2N rows, so the
    // window [N, 2N) is complete and nothing follows it.
    let page = page_of(rows(2 * N), N as u64, true);
    assert_eq!(page["truncated"], json!(false), "{page}");
    assert_eq!(page["returned_count"], json!(N));
    assert!(page.get("next_offset").is_none());
    assert_eq!(injected_fetch_limit(N as u64), 2 * N as u64 + 1);
}

#[test]
fn a_caller_limit_is_not_reported_as_truncated() {
    // The caller declared LIMIT N + 1: that bound is theirs, every row it
    // admits is returned, and no probe row is dropped.
    let page = page_of(rows(N + 1), 0, false);
    assert_eq!(page["truncated"], json!(false), "{page}");
    assert_eq!(page["total_count"], json!(N + 1));
    assert_eq!(page["returned_count"], json!(N + 1));
    assert!(page.get("row_limit").is_none(), "{page}");
    assert!(page.get("next_offset").is_none());
}

/// A graph on disk with `count` Function nodes whose ids sort in insertion
/// order (`f0000`, `f0001`, ...). The store handle is dropped so the read
/// path's cache opens its own.
fn seeded_graph(prefix: &str, count: usize) -> (tempfile::TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir");
    let db = dir.path().join("db");
    let store = GraphStore::open_or_create(&db).expect("open_or_create");
    store.create_schema().expect("create_schema");
    let nodes: Vec<Vec<(String, String)>> = (0..count)
        .map(|i| {
            let id = format!("f{i:04}");
            [
                ("id", id.clone()),
                ("name", id.clone()),
                ("qualified_name", format!("m.rs::{id}")),
                ("start_line", "1".to_string()),
                ("end_line", "1".to_string()),
                ("visibility", "pub".to_string()),
                ("is_async", "false".to_string()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
        })
        .collect();
    store
        .bulk_insert_nodes(NODE_FUNCTION, &nodes)
        .expect("seed");
    drop(store);
    let path = db.to_str().expect("utf-8 temp path").to_string();
    (dir, path)
}

fn query_page(graph: &str, query: &str, offset: u64) -> Value {
    do_query_graph(&json!({"graph_path": graph, "query": query, "offset": offset}))
        .unwrap_or_else(|e| panic!("query_graph failed at offset {offset}: {e}"))
}

#[test]
fn paging_walks_past_the_injected_row_limit_without_gap_or_duplicate() {
    let total = N + 25;
    let (_dir, graph) = seeded_graph("row_limit_walk", total);
    let query = "MATCH (n:Function) RETURN n.id ORDER BY n.id";

    let first = query_page(&graph, query, 0);
    assert_eq!(first["truncated"], json!(true), "{first}");
    assert_eq!(first["row_limit"], json!(N));

    let mut walked: Vec<String> = Vec::new();
    let mut offset = 0;
    loop {
        let page = query_page(&graph, query, offset);
        let cells = page["rows"].as_array().cloned().unwrap_or_default();
        walked.extend(cells.iter().filter_map(|r| r[0].as_str().map(String::from)));
        match page.get("next_offset").and_then(Value::as_u64) {
            Some(next) => {
                assert!(next > offset, "cursor must advance: {next} <= {offset}");
                offset = next;
            }
            None => break,
        }
    }
    let expected: Vec<String> = (0..total).map(|i| format!("f{i:04}")).collect();
    assert_eq!(walked, expected, "every row once, in order");

    // Exactly N matching rows through the engine: no cut, no cursor.
    let exact = query_page(
        &graph,
        &format!("MATCH (n:Function) WHERE n.id < 'f{N:04}' RETURN n.id"),
        0,
    );
    assert_eq!(exact["truncated"], json!(false), "{exact}");
    assert_eq!(exact["total_count"], json!(N));
    assert!(exact.get("next_offset").is_none());

    // A caller LIMIT keeps its meaning: no row_limit, no truncation.
    let mine = query_page(&graph, "MATCH (n:Function) RETURN n.id LIMIT 4", 0);
    assert_eq!(mine["limit_injected"], json!(false));
    assert_eq!(mine["total_count"], json!(4));
    assert_eq!(mine["truncated"], json!(false));
    assert!(mine.get("row_limit").is_none(), "{mine}");
}

/// The injected bound now grows with `offset` and saturates at `u64::MAX`. A
/// huge offset must stay an ordinary empty page, as it was when the bound was
/// a constant, rather than become an engine error over the printed literal.
#[test]
fn a_huge_offset_is_an_empty_page_not_an_engine_error() {
    let (_dir, graph) = seeded_graph("row_limit_huge_offset", 3);
    let page = query_page(&graph, "MATCH (n:Function) RETURN n.id", u64::MAX);
    assert_eq!(page["returned_count"], json!(0), "{page}");
    assert_eq!(page["truncated"], json!(false));
    assert!(page.get("next_offset").is_none());
}
