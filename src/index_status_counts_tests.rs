// index_status_counts_tests: issues #361 and #379. index_status only reads, so
// it reads through the cache's handle like the other read tools: a handle held
// by a running request neither refuses it nor is evicted by it, and a total
// that cannot be read is still an error, never zeros.

use super::run_index_status;
use crate::graph_cache::open_cached;
use crate::graph_store::GraphStore;
use serde_json::json;
use std::path::Path;
use std::rc::Rc;

fn seed(path: &Path) {
    let store = GraphStore::open_or_create(path).expect("open");
    store.create_schema().expect("schema");
    store
        .execute_query("CREATE (:Function {id: 'base', name: 'base'})")
        .expect("seed");
}

#[test]
fn index_status_reads_real_counts_while_the_graph_is_held() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    let args = json!({ "graph_path": path.to_string_lossy() });

    let held = open_cached(&path).expect("cached open");
    let answered = run_index_status(&args);
    assert_eq!(answered["status"], "ok", "a read must not be refused: {answered}");
    assert_eq!(answered["node_count"], 1, "{answered}");
    let again = open_cached(&path).expect("cached open");
    assert!(
        Rc::ptr_eq(&held, &again),
        "index_status must reuse the cached handle, not evict it"
    );
}

#[test]
fn index_status_on_a_missing_graph_is_an_error_and_creates_nothing() {
    let tmp = tempfile::tempdir().expect("tmp");
    let missing = tmp.path().join("no-such-graph");
    let args = json!({ "graph_path": missing.to_string_lossy() });
    let answered = run_index_status(&args);
    assert_eq!(answered["status"], "error", "{answered}");
    assert!(answered.get("node_count").is_none(), "{answered}");
    assert!(!missing.exists(), "a status read must not create a graph");
}
