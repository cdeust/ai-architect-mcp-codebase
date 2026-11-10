// resolver::retract_tests: issue #443, first resolver slice. After an
// incremental refresh, `resolve_graph` leaves the call rows and flags a full
// index of the same tree would have: a row of an earlier pass that this pass
// does not decide again is retracted, and its site reopened.

use crate::graph_store::GraphStore;
use crate::{indexer, resolver};
use std::fs;
use std::path::{Path, PathBuf};

struct Repo {
    _tmp: tempfile::TempDir,
    source: PathBuf,
    graph: PathBuf,
    manifest: PathBuf,
}

/// (is_resolved and open reason of the `beta` sites, per-site rows, function-level rows into a
/// `beta`), each sorted, for the graph at `graph`.
type State = (Vec<String>, Vec<String>, Vec<String>);

fn state(graph: &Path) -> State {
    let store = GraphStore::open_or_create(graph).expect("open");
    let column = |cypher: &str| -> Vec<String> {
        let mut rows: Vec<String> = store
            .execute_query(cypher)
            .expect(cypher)
            .rows
            .into_iter()
            .map(|r| r.join(" -> ").to_lowercase())
            .collect();
        rows.sort();
        rows
    };
    (
        column(
            "MATCH (c:CallSite) WHERE c.callee_name = 'beta' \
             RETURN c.id, c.is_resolved, c.unresolved_reason",
        ),
        column(
            "MATCH (c:CallSite)-[r:Calls_CallSite_Function]->(t:Function) \
             WHERE t.name = 'beta' RETURN c.id, t.id, r.resolution_method",
        ),
        column(
            "MATCH (f:Function)-[r:Calls_Function_Function]->(t:Function) \
             WHERE t.name = 'beta' RETURN f.id, t.id, r.resolution_method",
        ),
    )
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        for (name, text) in files {
            fs::write(source.join(name), text).unwrap();
        }
        let graph = tmp.path().join("graph");
        let manifest = tmp.path().join("manifest.json");
        indexer::index_codebase(&source, &graph).expect("index");
        indexer::write_full_manifest(&source, &manifest, &indexer::IndexOptions::default())
            .expect("manifest");
        resolve(&graph);
        Repo {
            _tmp: tmp,
            source,
            graph,
            manifest,
        }
    }

    fn refresh(&self, name: &str, text: &str) {
        fs::write(self.source.join(name), text).unwrap();
        let prior = indexer::manifest::load(&self.manifest).expect("manifest");
        indexer::index_incremental(
            &self.source,
            &self.graph,
            &self.manifest,
            &indexer::IndexOptions::default(),
            &prior,
        )
        .expect("incremental refresh");
        resolve(&self.graph);
    }

    /// The state of a from-scratch index and resolve of the current tree.
    fn fresh_state(&self) -> State {
        let graph = self._tmp.path().join("fresh");
        indexer::index_codebase(&self.source, &graph).expect("fresh index");
        resolve(&graph);
        state(&graph)
    }
}

fn resolve(graph: &Path) {
    let store = GraphStore::open_or_create(graph).expect("open");
    resolver::resolve_graph(&store).expect("resolve");
}

#[test]
fn a_call_made_ambiguous_by_an_added_file_loses_its_earlier_rows() {
    // Codex review of #478, P1: `a.rs` (unchanged) calls `beta()`, unique in
    // the first pass. An added file declares a second `beta`.
    let repo = Repo::new(&[
        ("a.rs", "pub fn alpha() {\n    beta();\n}\n"),
        ("b.rs", "pub fn beta() {}\n"),
    ]);
    let before = state(&repo.graph);
    assert_eq!(before.1.len(), 1, "control: the site resolves: {before:?}");
    assert_eq!(before.2.len(), 1, "control: {before:?}");

    repo.refresh("c.rs", "pub fn beta() {}\n");
    let fresh = repo.fresh_state();
    assert_ne!(fresh, before, "the fixture must change the answer");
    assert_eq!(state(&repo.graph), fresh);
}

#[test]
fn a_caller_keeps_its_function_level_row_while_another_site_still_resolves_to_it() {
    // Two sites of `alpha` call `beta`; only the qualified one survives the
    // second `beta`, so the function-level row stays and one per-site row goes.
    let repo = Repo::new(&[
        ("lib.rs", "mod a;\nmod b;\nmod c;\n"),
        (
            "a.rs",
            "use crate::b::beta;\npub fn alpha() {\n    beta();\n    crate::b::beta();\n}\n",
        ),
        ("b.rs", "pub fn beta() {}\n"),
        ("c.rs", "pub fn gamma() {}\n"),
    ]);
    repo.refresh("c.rs", "pub fn beta() {}\n");
    assert_eq!(state(&repo.graph), repo.fresh_state());
}

#[test]
fn a_second_resolve_of_an_unchanged_graph_changes_nothing() {
    let repo = Repo::new(&[
        ("a.rs", "pub fn alpha() {\n    beta();\n}\n"),
        ("b.rs", "pub fn beta() {}\n"),
    ]);
    let first = state(&repo.graph);
    resolve(&repo.graph);
    assert_eq!(state(&repo.graph), first);
    assert_eq!(first, repo.fresh_state());
}

#[test]
fn a_call_made_unique_again_by_a_deleted_duplicate_is_resolved_as_a_fresh_index_would() {
    // The reverse direction, already handled before this slice by the purge
    // (`stale_sites`) and the pass re-deciding every site; kept as a guard.
    let repo = Repo::new(&[
        ("a.rs", "pub fn alpha() {\n    beta();\n}\n"),
        ("b.rs", "pub fn beta() {}\n"),
        ("c.rs", "pub fn beta() {}\n"),
    ]);
    repo.refresh("c.rs", "pub fn gamma() {}\n");
    assert_eq!(state(&repo.graph), repo.fresh_state());
}

#[test]
fn a_retracted_caller_row_keeps_its_runtime_count_as_an_observed_call() {
    // Codex review of #496: trace ingest's `observed_count` lives on the static
    // row; retracting the row moves it to the row ingest writes when there is
    // no static one.
    let repo = Repo::new(&[
        ("a.rs", "pub fn alpha() {\n    beta();\n}\n"),
        ("b.rs", "pub fn beta() {}\n"),
    ]);
    {
        let store = GraphStore::open_or_create(&repo.graph).expect("open");
        store
            .execute_query(
                "MATCH (a:Function {id: 'a.rs::alpha'})-[r:Calls_Function_Function]->\
                 (b:Function {id: 'b.rs::beta'}) SET r.observed_count = 7",
            )
            .expect("annotate, as trace ingest does");
    }
    repo.refresh("c.rs", "pub fn beta() {}\n");
    let store = GraphStore::open_or_create(&repo.graph).expect("open");
    let observed = store
        .execute_query(
            "MATCH (a:Function {id: 'a.rs::alpha'})-[r:OBSERVED_CALLS_Function_Function]->\
             (b:Function {id: 'b.rs::beta'}) RETURN r.observed_count",
        )
        .expect("observed")
        .rows;
    assert_eq!(observed, [["7".to_string()]]);
}
