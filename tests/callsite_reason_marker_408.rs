//! Issue #408: `reasons_recorded` vouches for every open call site of the
//! graph, so it may only be true when every `CallSite` row was written under
//! the current form. A graph indexed before the form (its unchanged files keep
//! `callee_shape = ''`, no indirect call recorded) that goes through an
//! incremental refresh and a standalone `resolve_graph` must keep reading
//! `reasons_recorded: false`; only a full index restores it.
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, manifest, IndexOptions};
use ai_architect_mcp::resolver;
use std::fs;
use std::path::{Path, PathBuf};

fn write_fixture(repo: &Path) {
    fs::create_dir_all(repo.join("src")).expect("mk src");
    fs::write(
        repo.join("src/a.py"),
        "def a():\n    return b()\n\ndef b():\n    return missing()\n",
    )
    .expect("write a.py");
    fs::write(repo.join("src/c.py"), "def c():\n    return 1\n").expect("write c.py");
}

/// Full index of `repo` into `tmp/out`; returns (graph, manifest).
fn full_index(tmp: &Path, repo: &Path) -> (PathBuf, PathBuf) {
    let out = tmp.join("out");
    fs::create_dir_all(&out).expect("mk out");
    let graph = out.join("graph");
    let manifest_path = manifest::manifest_path(&out);
    indexer::index_codebase_with_language(repo, &graph, &IndexOptions::default())
        .expect("full index");
    indexer::write_full_manifest(repo, &manifest_path, &IndexOptions::default())
        .expect("write manifest");
    (graph, manifest_path)
}

fn incremental_after_edit(repo: &Path, graph: &Path, manifest_path: &Path) {
    fs::write(repo.join("src/c.py"), "def c():\n    return 2\n").expect("edit c.py");
    let prior = manifest::load(manifest_path).expect("manifest loads");
    let inc =
        indexer::index_incremental(repo, graph, manifest_path, &IndexOptions::default(), &prior)
            .expect("incremental pass");
    assert_eq!(inc.changed, 1, "only c.py changed");
}

fn resolve_and_read(graph: &Path) -> bool {
    let store = GraphStore::open_or_create(graph).expect("open graph");
    resolver::resolve_graph(&store).expect("standalone resolve");
    store
        .unresolved_site_summary()
        .expect("summary")
        .reasons_recorded
}

/// Turns a freshly indexed graph into one written before the form: no marker of
/// the form and the rows of a parser that recorded no `callee_shape`.
fn age_the_graph(graph: &Path) {
    let store = GraphStore::open_or_create(graph).expect("open graph");
    store
        .execute_query(
            "MATCH (m:GraphMarker) WHERE m.id IN ['callsite_rows_form', 'callsite_reason_form'] \
             DELETE m",
        )
        .expect("drop the form markers");
    store
        .execute_query("MATCH (cs:CallSite) SET cs.callee_shape = ''")
        .expect("blank the shapes");
}

#[test]
fn an_incremental_refresh_and_a_standalone_resolve_do_not_vouch_for_old_rows() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("repo");
    write_fixture(&repo);
    let (graph, manifest_path) = full_index(tmp.path(), &repo);
    age_the_graph(&graph);
    incremental_after_edit(&repo, &graph, &manifest_path);
    assert!(
        !resolve_and_read(&graph),
        "a.py's rows predate the form: the marker must not be written"
    );
}

#[test]
fn a_standalone_resolve_on_an_old_graph_does_not_vouch_either() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("repo");
    write_fixture(&repo);
    let (graph, _manifest) = full_index(tmp.path(), &repo);
    age_the_graph(&graph);
    assert!(!resolve_and_read(&graph));
}

#[test]
fn a_full_index_restores_the_marker_and_an_incremental_refresh_keeps_it() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("repo");
    write_fixture(&repo);
    let (graph, manifest_path) = full_index(tmp.path(), &repo);
    assert!(resolve_and_read(&graph), "a full index writes current rows");
    incremental_after_edit(&repo, &graph, &manifest_path);
    assert!(
        resolve_and_read(&graph),
        "an incremental pass over current rows keeps them current"
    );
}
