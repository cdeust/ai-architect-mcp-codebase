//! The durability check of the LSP pass (issue #352), shared by the two ways
//! the pass runs: the standalone `lsp_resolve` tool and the LSP phase of
//! `analyze_codebase`. Both write through the same edge writer, so both close
//! their handle, reopen the graph and compare before they answer `completed`.

use crate::graph_store::GraphStore;
use std::path::Path;

/// `resolution_method` the LSP pass writes on every edge and per-site row.
// source: src/lsp_resolver/edges.rs, the `resolution_method` column it writes.
pub(super) const LSP_RESOLUTION_METHOD: &str = "lsp-definition";

/// Counts the LSP rows through the handle that wrote them, closes that handle,
/// reopens the graph and counts again. A difference means rows the pass wrote
/// are not there for the next reader (#352 lost them silently while the
/// `is_resolved` flags stayed true): an error, never a `completed` answer.
/// Returns the number of durable rows.
pub(super) fn verify_after_reopen(store: GraphStore, graph_path: &Path) -> Result<u64, String> {
    let written = store.count_edges_by_method(LSP_RESOLUTION_METHOD)?;
    drop(store);
    let durable =
        GraphStore::open_or_create(graph_path)?.count_edges_by_method(LSP_RESOLUTION_METHOD)?;
    compare_counts(written, durable, graph_path)
}

/// `Ok(durable)` when the reopened graph holds every row the pass wrote.
fn compare_counts(written: u64, durable: u64, graph_path: &Path) -> Result<u64, String> {
    if durable == written {
        Ok(durable)
    } else {
        Err(format!(
            "lsp_rows_not_durable: the pass holds {written} lsp-definition rows, but only \
             {durable} are readable after reopening the graph at {}",
            graph_path.display()
        ))
    }
}

// Deterministic stand-in for the issue's sequence, run in CI without a language
// server: `query_graph`, then a write through the store method the LSP edge
// writer uses (`insert_edge_if_absent`, src/lsp_resolver/edges.rs), the
// durability check, the end of the session, and a reopen.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_cache;
    use crate::graph_store;
    use serde_json::json;

    fn indexed_graph() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().expect("tmp");
        let crate_dir = tmp.path().join("crate");
        std::fs::create_dir_all(crate_dir.join("src")).expect("mkdir");
        std::fs::write(
            crate_dir.join("src/lib.rs"),
            "pub fn caller() { callee(); }\npub fn callee() {}\n",
        )
        .expect("source");
        let graph = tmp.path().join("graph");
        crate::indexer::index_codebase(&crate_dir, &graph).expect("index");
        (tmp, graph)
    }

    fn query(graph: &Path, cypher: &str) -> serde_json::Value {
        crate::query_handlers::do_query_graph(
            &json!({"graph_path": graph, "query": cypher, "format": "tabular"}),
        )
        .expect("query_graph")
    }

    fn function_ids(graph: &Path) -> Vec<String> {
        let answer = query(graph, "MATCH (f:Function) RETURN f.id ORDER BY f.id");
        answer["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| r[0].as_str().expect("id").to_string())
            .collect()
    }

    fn write_lsp_edge(store: &GraphStore, from: &str, to: &str) {
        store
            .insert_edge_if_absent(
                "Calls_Function_Function",
                from,
                to,
                &[
                    ("confidence", "0.9"),
                    ("resolution_method", "'lsp-definition'"),
                ],
            )
            .expect("write edge");
    }

    fn durable_rows(graph: &Path) -> u64 {
        graph_store::GraphStore::open_or_create(graph)
            .expect("reopen")
            .count_edges_by_method(LSP_RESOLUTION_METHOD)
            .expect("count")
    }

    #[test]
    fn a_query_before_the_lsp_write_does_not_cost_its_rows() {
        let (_tmp, graph) = indexed_graph();
        let ids = function_ids(&graph);
        assert_eq!(ids.len(), 2, "{ids:?}");

        let store = graph_store::GraphStore::open_or_create(&graph).expect("write handle");
        write_lsp_edge(&store, &ids[0], &ids[1]);
        assert_eq!(verify_after_reopen(store, &graph), Ok(1));

        // A read after the write, then the end of the session: the cached
        // handle is dropped with the process.
        assert_eq!(function_ids(&graph).len(), 2);
        graph_store::release_open_handles(&graph).expect("end of session");
        assert_eq!(durable_rows(&graph), 1);
    }

    #[test]
    fn a_query_in_the_middle_of_the_write_cannot_swallow_it() {
        let (_tmp, graph) = indexed_graph();
        let ids = function_ids(&graph);
        let store = graph_store::GraphStore::open_or_create(&graph).expect("write handle");
        write_lsp_edge(&store, &ids[0], &ids[1]);
        // The cached read handle opens while the writer is live and is released
        // when the writer verifies: the row must still be there.
        let _ = graph_cache::open_cached(&graph).map(drop);
        assert_eq!(verify_after_reopen(store, &graph), Ok(1));
        assert_eq!(durable_rows(&graph), 1);
    }

    #[test]
    fn a_request_holding_the_cached_handle_makes_the_lsp_write_fail_loudly() {
        let (_tmp, graph) = indexed_graph();
        let held = graph_cache::open_cached(&graph).expect("cached");
        let refused = match graph_store::GraphStore::open_or_create(&graph) {
            Err(e) => e,
            Ok(_) => panic!("must refuse while a request holds the handle"),
        };
        assert!(refused.contains("graph_handle_in_use"), "{refused}");
        drop(held);
    }

    #[test]
    fn a_shortfall_of_durable_rows_is_an_error_naming_the_graph() {
        let path = Path::new("/graphs/g");
        assert_eq!(compare_counts(2, 2, path), Ok(2));
        assert_eq!(compare_counts(0, 0, path), Ok(0));
        let lost = compare_counts(5, 0, path).expect_err("5 written, 0 durable");
        assert!(lost.starts_with("lsp_rows_not_durable"), "{lost}");
        assert!(lost.contains("5") && lost.contains("/graphs/g"), "{lost}");
        assert!(
            compare_counts(1, 2, path).is_err(),
            "more durable than written is also a mismatch"
        );
    }
}
