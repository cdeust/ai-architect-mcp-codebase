//! Issue #353, second review round: `CallSite.is_resolved` after a purge.
//!
//! An incremental refresh purges the target with its file and leaves the flag of
//! the unchanged caller's site as it was (`true`). The next resolve found no
//! target ("no target found") yet left the flag, so the graph claimed a
//! resolution it no longer had. The reset applies to every site the resolver
//! leaves open, but never to a site another tier resolved and holds an edge for.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::fs;
use std::path::PathBuf;

struct Fixture {
    _tmp: tempfile::TempDir,
    source: PathBuf,
    graph: PathBuf,
    manifest: PathBuf,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
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
        let f = Fixture {
            _tmp: tmp,
            source,
            graph,
            manifest,
        };
        f.resolve();
        f
    }

    fn store(&self) -> GraphStore {
        GraphStore::open_or_create(&self.graph).expect("open")
    }

    fn resolve(&self) -> resolver::ResolutionResult {
        resolver::resolve_graph(&self.store()).expect("resolve")
    }

    fn refresh(&self) {
        let prior = indexer::manifest::load(&self.manifest).expect("manifest");
        indexer::index_incremental(
            &self.source,
            &self.graph,
            &self.manifest,
            &indexer::IndexOptions::default(),
            &prior,
        )
        .expect("incremental refresh");
    }

    fn flag(&self, callee: &str) -> String {
        let rows = self
            .store()
            .execute_query(&format!(
                "MATCH (c:CallSite) WHERE c.callee_name = '{callee}' RETURN c.is_resolved"
            ))
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        rows[0][0].to_lowercase()
    }
}

/// The exact repro of the review: `a.rs` is deleted, `b.rs` is unchanged.
#[test]
fn a_site_whose_target_was_purged_is_unresolved_after_the_next_resolve() {
    let f = Fixture::new(&[
        ("a.rs", "pub fn helper() {}\n"),
        ("b.rs", "fn f() {\n    helper();\n}\n"),
    ]);
    assert_eq!(
        f.flag("helper"),
        "true",
        "control: resolved before the purge"
    );
    fs::remove_file(f.source.join("a.rs")).unwrap();
    f.refresh();
    let result = f.resolve();
    assert!(
        result
            .unresolved
            .iter()
            .any(|u| u.target_text == "helper" && u.reason == "no target found"),
        "the resolver did not report the site as unresolved"
    );
    assert_eq!(
        f.flag("helper"),
        "false",
        "the graph claims a resolution it lost"
    );
}

/// A site that another tier (the language server) resolved keeps its flag when
/// the static resolver still finds no target: an edge is the evidence, and the
/// reset must not undo it.
#[test]
fn a_site_resolved_by_another_tier_keeps_its_flag() {
    let f = Fixture::new(&[
        ("a.rs", "pub fn real() {}\n"),
        ("b.rs", "fn f() {\n    alias();\n}\n"),
    ]);
    assert_eq!(
        f.flag("alias"),
        "false",
        "control: the static tier finds nothing"
    );
    let store = f.store();
    store
        .execute_query(
            "MATCH (c:CallSite), (t:Function) WHERE c.callee_name = 'alias' AND t.name = 'real' \
             CREATE (c)-[:Calls_CallSite_Function {confidence: 0.9, resolution_method: 'lsp'}]->(t)",
        )
        .expect("lsp edge");
    store
        .execute_query("MATCH (c:CallSite) WHERE c.callee_name = 'alias' SET c.is_resolved = true")
        .expect("flag");
    drop(store);
    f.resolve();
    assert_eq!(
        f.flag("alias"),
        "true",
        "the reset undid another tier's resolution"
    );
}
