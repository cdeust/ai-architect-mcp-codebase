// lsp_resolver::purge_reset_tests: issue #353, third review round. A call site
// the language server pass resolved (flag + `lsp-definition` rows, written by
// `insert_lsp_edge` and `mark_nodes_resolved`, the persistence the pass itself
// calls) keeps both through an incremental refresh that does not touch its
// target, and loses both only when the refresh purges the target.

use super::edges::insert_lsp_edge;
use super::sites::{NodePosition, UnresolvedCallSite};
use crate::graph_store::GraphStore;
use crate::{indexer, resolver};
use std::fs;
use std::path::PathBuf;

struct Repo {
    _tmp: tempfile::TempDir,
    source: PathBuf,
    graph: PathBuf,
    manifest: PathBuf,
}

impl Repo {
    /// `a.rs` defines `real`; `b.rs` calls `alias()`, which no static tier finds;
    /// the language server tier then resolves that site to `real`.
    fn with_lsp_site() -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("a.rs"), "pub fn real() {}\n").unwrap();
        fs::write(source.join("b.rs"), "fn f() {\n    alias();\n}\n").unwrap();
        fs::write(source.join("c.rs"), "fn unrelated() {}\n").unwrap();
        let graph = tmp.path().join("graph");
        let manifest = tmp.path().join("manifest.json");
        indexer::index_codebase(&source, &graph).expect("index");
        indexer::write_full_manifest(&source, &manifest, &indexer::IndexOptions::default())
            .expect("manifest");
        let repo = Repo {
            _tmp: tmp,
            source,
            graph,
            manifest,
        };
        let store = repo.store();
        resolver::resolve_graph(&store).expect("resolve");
        let id = store
            .execute_query("MATCH (c:CallSite) WHERE c.callee_name = 'alias' RETURN c.id")
            .unwrap()
            .rows[0][0]
            .clone();
        let site = UnresolvedCallSite {
            id: id.clone(),
            caller_qn: "b.rs::f".into(),
            caller_label: "Function".into(),
            callee_name: "alias".into(),
            file_path: "b.rs".into(),
            line: 2,
            col: 4,
        };
        let target = NodePosition {
            id: "a.rs::real".into(),
            label: "Function".into(),
        };
        assert!(insert_lsp_edge(
            &store,
            "Calls_Function_Function",
            &site,
            &target
        ));
        store
            .mark_nodes_resolved("CallSite", &[id.as_str()])
            .unwrap();
        drop(store);
        repo
    }

    fn store(&self) -> GraphStore {
        GraphStore::open_or_create(&self.graph).expect("open")
    }

    fn refresh_after(&self, name: &str, text: &str) {
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
        resolver::resolve_graph(&self.store()).expect("resolve");
    }

    /// (flag, per-site rows, function-level rows) of the `alias` site.
    fn state(&self) -> (String, usize, usize) {
        let store = self.store();
        let flag = store
            .execute_query("MATCH (c:CallSite) WHERE c.callee_name = 'alias' RETURN c.is_resolved")
            .unwrap()
            .rows[0][0]
            .to_lowercase();
        let per_site = store
            .execute_query("MATCH (c:CallSite)-[r:Calls_CallSite_Function]->() WHERE c.callee_name = 'alias' RETURN r.resolution_method")
            .unwrap()
            .rows
            .len();
        let function_level = store
            .execute_query("MATCH (:Function)-[r:Calls_Function_Function]->(t:Function) WHERE t.name = 'real' RETURN r.resolution_method")
            .unwrap()
            .rows
            .len();
        (flag, per_site, function_level)
    }
}

#[test]
fn an_lsp_resolved_site_keeps_its_flag_and_rows_when_an_unrelated_file_changes() {
    let repo = Repo::with_lsp_site();
    assert_eq!(repo.state(), ("true".to_string(), 1, 1), "control");
    repo.refresh_after("c.rs", "fn unrelated() {}\nfn touched() {}\n");
    assert_eq!(repo.state(), ("true".to_string(), 1, 1));
}

#[test]
fn an_lsp_resolved_site_keeps_both_when_its_target_file_is_rewritten_and_the_target_survives() {
    let repo = Repo::with_lsp_site();
    repo.refresh_after("a.rs", "pub fn real() {}\npub fn more() {}\n");
    assert_eq!(repo.state(), ("true".to_string(), 1, 1));
}

#[test]
fn an_lsp_resolved_site_is_reopened_when_its_target_is_purged() {
    let repo = Repo::with_lsp_site();
    repo.refresh_after("a.rs", "pub fn other() {}\n");
    assert_eq!(repo.state(), ("false".to_string(), 0, 0));
}
