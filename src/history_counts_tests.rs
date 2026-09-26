// history_counts_tests: issue #361. The two responses that report a graph's
// totals after a write, the bootstrap responses (through `count_fields`) and
// the incremental response with an export, must not turn a refused read into
// zeros. A running request's cached handle on the graph makes the read refuse.

use super::{count_fields, finish_incremental_response};
use crate::artifact::{ARTIFACT_DIR, ARTIFACT_FILE};
use crate::graph_cache::open_cached;
use crate::graph_store::GraphStore;
use crate::indexer::IncrementalResult;
use std::path::Path;

fn seed(path: &Path) {
    let store = GraphStore::open_or_create(path).expect("open");
    store.create_schema().expect("schema");
    store
        .execute_query("CREATE (:Function {id: 'base', name: 'base'})")
        .expect("seed");
}

fn incremental(graph: &Path) -> IncrementalResult {
    IncrementalResult {
        graph_path: graph.to_path_buf(),
        changed: 1,
        added: 0,
        deleted: 0,
        renamed: 0,
        unchanged: 0,
        files_reparsed: 1,
        elapsed_ms: 1,
    }
}

#[test]
fn count_fields_reports_the_reason_instead_of_zeros_while_the_graph_is_held() {
    let tmp = tempfile::tempdir().expect("tmp");
    let graph = tmp.path().join("g");
    seed(&graph);
    let held = open_cached(&graph).expect("cached open");
    let fields = count_fields(&graph);
    drop(held);
    let reason = fields["counts_unavailable"].as_str().unwrap_or_default();
    assert!(reason.contains("graph_handle_in_use"), "{fields}");
    assert!(fields.get("node_count").is_none(), "{fields}");
    assert_eq!(count_fields(&graph)["node_count"], 1);
}

#[test]
fn an_incremental_export_is_skipped_rather_than_record_unread_totals() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (codebase, out) = (tmp.path().join("code"), tmp.path().join("out"));
    std::fs::create_dir_all(&codebase).expect("codebase");
    std::fs::create_dir_all(&out).expect("output dir");
    let graph = out.join("graph");
    seed(&graph);
    let artifact = codebase.join(ARTIFACT_DIR).join(ARTIFACT_FILE);
    std::fs::create_dir_all(artifact.parent().expect("dir")).expect("artifact dir");
    std::fs::write(&artifact, b"previous artifact").expect("prior artifact");

    let held = open_cached(&graph).expect("cached open");
    let manifest = out.join("file_manifest.json");
    let response = finish_incremental_response(
        incremental(&graph),
        &graph,
        &codebase,
        &manifest,
        true,
        false,
    );
    drop(held);

    assert_eq!(
        response["status"], "ok",
        "the index itself succeeded: {response}"
    );
    let reason = response["counts_unavailable"].as_str().unwrap_or_default();
    assert!(reason.contains("graph_handle_in_use"), "{response}");
    assert!(response["artifact_error"].is_string(), "{response}");
    assert!(response.get("node_count").is_none(), "{response}");
    assert!(response.get("artifact_path").is_none(), "{response}");
    assert_eq!(
        std::fs::read(&artifact).expect("artifact kept"),
        b"previous artifact",
        "the previous artifact must be left untouched"
    );
}
