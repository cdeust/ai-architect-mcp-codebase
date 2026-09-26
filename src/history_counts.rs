// history_counts: the node and relationship totals of an on-disk graph as the
// history and indexing responses report them (issues #338, #361).

use crate::graph_store;
use serde_json::{json, Value};
use std::path::Path;

/// The count fields of a response whose graph is already in place (issue
/// #361): the three totals, or `counts_unavailable` with the reason when the
/// graph cannot be read, for example while a running request holds its handle
/// (`graph_handle_in_use`). A total that was not read is never reported as
/// zero, since zero reads as an empty graph.
pub(crate) fn count_fields(graph_dir: &Path) -> Value {
    match try_graph_counts(graph_dir) {
        Ok(c) => json!({
            "node_count": c.nodes,
            "edge_count": c.edges,
            "call_site_target_count": c.call_site_targets,
        }),
        Err(e) => json!({ "counts_unavailable": e }),
    }
}

/// Merges the fields of `extra` (an object) into `resp`.
pub(crate) fn merge_fields(resp: &mut Value, extra: Value) {
    if let (Some(target), Value::Object(fields)) = (resp.as_object_mut(), extra) {
        target.extend(fields);
    }
}

/// Node and relationship totals, or the reason when the graph cannot be opened
/// or queried.
pub(crate) fn try_graph_counts(graph_dir: &Path) -> Result<graph_store::GraphCounts, String> {
    if !graph_dir.exists() {
        return Err(format!("graph not found: {}", graph_dir.display()));
    }
    graph_store::GraphStore::open_or_create(graph_dir)?.graph_counts()
}

#[cfg(test)]
mod graph_counts_tests {
    use super::try_graph_counts;

    #[test]
    fn a_missing_graph_path_is_an_error_and_is_not_created() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let missing = tmp.path().join("no-such-graph");
        assert!(try_graph_counts(&missing).is_err());
        assert!(!missing.exists(), "a read must not create a graph");
    }
}
