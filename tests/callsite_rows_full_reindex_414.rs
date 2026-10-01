//! Issue #414: a full index over a graph directory that already holds an index.
//! The rows marker written at its end vouches for every `CallSite` row of the
//! graph, so no row of an earlier index may survive it. And a standalone
//! `resolve_graph` over a graph written before the marker table existed has no
//! marker to withdraw: it must resolve, not fail on the missing table.
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, IndexOptions};
use ai_architect_mcp::resolver;
use std::fs;
use std::path::Path;

fn write_repo(repo: &Path, file: &str, caller: &str, callee: &str) {
    fs::create_dir_all(repo).expect("mk repo");
    fs::write(
        repo.join(file),
        format!("def {caller}():\n    return {callee}()\n\ndef {callee}():\n    return 1\n"),
    )
    .expect("write source");
}

fn call_site_files(graph: &Path) -> Vec<String> {
    let store = GraphStore::open_or_create(graph).expect("open graph");
    let mut ids: Vec<String> = store
        .execute_query("MATCH (c:CallSite) RETURN c.id")
        .expect("read sites")
        .rows
        .into_iter()
        .map(|r| r[0].split("::").next().unwrap_or_default().to_string())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[test]
fn a_full_index_over_an_existing_graph_leaves_no_call_site_of_the_earlier_index() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (first, second) = (tmp.path().join("first"), tmp.path().join("second"));
    write_repo(&first, "a.py", "f", "g");
    write_repo(&second, "b.py", "h", "k");
    let graph = tmp.path().join("graph");
    let options = IndexOptions::default();
    indexer::index_codebase_with_language(&first, &graph, &options).expect("first index");
    assert_eq!(call_site_files(&graph), ["a.py"]);

    // No file in common: the second index succeeds over the first one's tables.
    indexer::index_codebase_with_language(&second, &graph, &options).expect("second index");

    assert_eq!(
        call_site_files(&graph),
        ["b.py"],
        "a.py's call sites were written by the first index and must not outlive the second"
    );
    let store = GraphStore::open_or_create(&graph).expect("open graph");
    assert!(
        store.has_callsite_rows(),
        "the second index wrote the marker"
    );
}

#[test]
fn resolving_a_graph_without_the_marker_table_succeeds_and_vouches_for_nothing() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("repo");
    write_repo(&repo, "a.py", "f", "g");
    let graph = tmp.path().join("graph");
    indexer::index_codebase_with_language(&repo, &graph, &IndexOptions::default()).expect("index");
    let store = GraphStore::open_or_create(&graph).expect("open graph");
    store
        .execute_query("DROP TABLE GraphMarker")
        .expect("age the graph: no marker table");

    resolver::resolve_graph(&store).expect("resolve must not fail on the missing marker table");

    assert!(
        !store
            .unresolved_site_summary()
            .expect("summary")
            .reasons_recorded,
        "a graph without markers vouches for no reason"
    );
}
