// index_status_counts_tests: issue #361. A graph whose totals cannot be read
// must not be reported as an empty graph. Since #360 an open of a graph whose
// cached handle is held by a running request is refused; index_status used to
// swallow that refusal and answer zeros with `status: ok`.

use super::run_index_status;
use crate::graph_cache::open_cached;
use crate::graph_store::GraphStore;
use serde_json::json;
use std::path::Path;

fn seed(path: &Path) {
    let store = GraphStore::open_or_create(path).expect("open");
    store.create_schema().expect("schema");
    store
        .execute_query("CREATE (:Function {id: 'base', name: 'base'})")
        .expect("seed");
}

#[test]
fn index_status_does_not_report_zeros_when_the_graph_is_held() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    let args = json!({ "graph_path": path.to_string_lossy() });

    let held = open_cached(&path).expect("cached open");
    let refused = run_index_status(&args);
    assert_ne!(
        refused["status"], "ok",
        "a refused read is not a measurement: {refused}"
    );
    assert!(
        refused.get("node_count").is_none(),
        "no count may be reported when none was read: {refused}"
    );
    let message = refused["message"].as_str().unwrap_or_default();
    assert!(message.contains("graph_handle_in_use"), "{refused}");
    drop(held);

    let answered = run_index_status(&args);
    assert_eq!(answered["status"], "ok", "{answered}");
    assert_eq!(answered["node_count"], 1, "{answered}");
}
