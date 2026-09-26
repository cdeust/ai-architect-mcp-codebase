// trace_ingest_handles_tests: ingest_traces writes OBSERVED_CALLS edges, so it
// is a write tool and falls under the rule of #352: a write must never go
// through, or next to, a handle that another request still holds, because the
// handle that closes last checkpoints its view over the other one.

use super::run_ingest_traces;
use crate::graph_cache::open_cached;
use crate::graph_store::GraphStore;
use serde_json::json;
use std::path::Path;

fn seed(path: &Path) {
    let store = GraphStore::open_or_create(path).expect("open");
    store.create_schema().expect("schema");
    store
        .execute_query("CREATE (:Function {id: 'a', name: 'a'}), (:Function {id: 'b', name: 'b'})")
        .expect("seed");
}

fn observed(path: &Path) -> String {
    GraphStore::open_or_create(path)
        .expect("fresh open")
        .execute_query("MATCH ()-[r:OBSERVED_CALLS_Function_Function]->() RETURN count(r)")
        .expect("count")
        .rows[0][0]
        .clone()
}

fn ingest(path: &Path) -> serde_json::Value {
    run_ingest_traces(&json!({
        "graph_path": path.to_string_lossy(),
        "traces": [{ "caller": "a", "callee": "b", "count": 3 }],
    }))
}

/// Releases every cached handle, as a write tool does before it opens.
fn release_all(path: &Path) {
    crate::graph_store::release_open_handles(path).expect("release");
}

#[test]
fn ingest_is_refused_while_another_request_holds_the_graph() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    let held = open_cached(&path).expect("cached open");
    let response = ingest(&path);
    assert!(
        response.to_string().contains("graph_handle_in_use"),
        "a write next to a held handle must be refused: {response}"
    );
    drop(held);
    release_all(&path);
    assert_eq!(observed(&path), "0", "a refused ingest writes nothing");
}

#[test]
fn ingested_rows_survive_a_later_read_and_a_release() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    // A read first puts a handle in the cache, as a client usually does.
    drop(open_cached(&path).expect("cached open"));
    let response = ingest(&path);
    assert_eq!(response["status"], "ok", "{response}");
    assert_eq!(response["unmatched_created"], 1, "{response}");
    // A read after the write reopens the graph through the cache.
    drop(open_cached(&path).expect("cached read after the write"));
    release_all(&path);
    assert_eq!(observed(&path), "1", "the ingested edge must be durable");
}
