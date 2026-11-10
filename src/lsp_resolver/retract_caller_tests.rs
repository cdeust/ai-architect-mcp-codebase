// lsp_resolver::retract_caller_tests: issue #443, Codex review of #496. When a
// caller reaches one target through a static site and a language-server site,
// the caller-level row keeps the static method (`insert_lsp_edge` writes only
// an absent row). If an incremental refresh makes the static site ambiguous,
// the resolver retracts its per-site row but must keep the caller-level row the
// language-server site still supports.

use super::edges::insert_lsp_edge;
use super::sites::{NodePosition, UnresolvedCallSite};
use crate::graph_store::GraphStore;
use crate::{indexer, resolver};
use std::fs;

fn column(store: &GraphStore, cypher: &str) -> Vec<String> {
    let mut rows: Vec<String> = store
        .execute_query(cypher)
        .expect(cypher)
        .rows
        .into_iter()
        .map(|r| r.join(" ").to_lowercase())
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_caller_level_row_a_language_server_site_supports_survives_the_retraction() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("a.rs"),
        "pub fn alpha() {\n    beta();\n    alias();\n}\n",
    )
    .unwrap();
    fs::write(source.join("b.rs"), "pub fn beta() {}\n").unwrap();
    let graph = tmp.path().join("graph");
    let manifest = tmp.path().join("manifest.json");
    let options = indexer::IndexOptions::default();
    indexer::index_codebase(&source, &graph).expect("index");
    indexer::write_full_manifest(&source, &manifest, &options).expect("manifest");
    {
        let store = GraphStore::open_or_create(&graph).expect("open");
        resolver::resolve_graph(&store).expect("resolve");
        let id = store
            .execute_query("MATCH (c:CallSite) WHERE c.callee_name = 'alias' RETURN c.id")
            .unwrap()
            .rows[0][0]
            .clone();
        let site = UnresolvedCallSite {
            id: id.clone(),
            caller_qn: "a.rs::alpha".into(),
            caller_label: "Function".into(),
            callee_name: "alias".into(),
            file_path: "a.rs".into(),
            line: 3,
            col: 4,
        };
        let target = NodePosition {
            id: "b.rs::beta".into(),
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
        let caller = column(
            &store,
            "MATCH (:Function {id: 'a.rs::alpha'})-[r:Calls_Function_Function]->(t) \
             RETURN t.id, r.resolution_method",
        );
        assert_eq!(caller, ["b.rs::beta unique-match"], "control");
    }

    fs::write(source.join("c.rs"), "pub fn beta() {}\n").unwrap();
    let prior = indexer::manifest::load(&manifest).expect("manifest");
    indexer::index_incremental(&source, &graph, &manifest, &options, &prior).expect("refresh");
    let store = GraphStore::open_or_create(&graph).expect("open");
    resolver::resolve_graph(&store).expect("resolve");

    let caller = column(
        &store,
        "MATCH (:Function {id: 'a.rs::alpha'})-[r:Calls_Function_Function]->(t) RETURN t.id",
    );
    assert_eq!(
        caller,
        ["b.rs::beta"],
        "the language-server site still supports it"
    );
    let sites = column(
        &store,
        "MATCH (c:CallSite) WHERE c.callee_name IN ['beta', 'alias'] \
         RETURN c.callee_name, c.is_resolved",
    );
    assert_eq!(sites, ["alias true", "beta false"]);
    let reason = column(
        &store,
        "MATCH (c:CallSite) WHERE c.callee_name = 'beta' RETURN c.unresolved_reason",
    );
    assert_ne!(reason, [""], "the reopened site carries its open reason");
}
