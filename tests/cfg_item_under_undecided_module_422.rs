//! Issue #422: the verdict of an item is read against its own file. An item
//! under a module whose gate the default build does not decide (`#[cfg(unix)]
//! mod m;`) keeps the verdict of its own gate; the module's own verdict is on
//! the `File` node, `unknown`. So the twins of one item in that file (`cfg(kani)`
//! and `cfg(not(kani))`) are still told apart, and a call among them is still
//! resolved to the twin the default build compiles, given that the file is.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::fs;

fn rows(store: &GraphStore, cypher: &str) -> Vec<Vec<String>> {
    store.execute_query(cypher).expect(cypher).rows
}

fn fixture(module_gate: &str) -> (tempfile::TempDir, GraphStore) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("crate");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fx422\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        format!("#[cfg({module_gate})]\nmod m;\n"),
    )
    .unwrap();
    fs::write(
        root.join("src/m.rs"),
        "#[cfg(kani)]\nfn g() {}\n#[cfg(not(kani))]\nfn g() {}\nfn h() {\n    g();\n}\n",
    )
    .unwrap();
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&root, &graph).expect("index");
    let store = GraphStore::open_or_create(&graph).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    (tmp, store)
}

fn twin_states(store: &GraphStore) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = rows(
        store,
        "MATCH (f:Function) WHERE f.name = 'g' RETURN f.cfg_gate, f.cfg_active",
    )
    .into_iter()
    .map(|r| (r[0].clone(), r[1].clone()))
    .collect();
    out.sort();
    out
}

fn assert_contract(module_gate: &str) {
    let (_tmp, store) = fixture(module_gate);
    assert_eq!(
        rows(
            &store,
            "MATCH (f:File) WHERE f.id ENDS WITH 'm.rs' RETURN f.cfg_active"
        ),
        [["unknown"]],
        "`{module_gate}` is not decided by the default profile: the module's verdict says so"
    );
    let states = twin_states(&store);
    assert_eq!(states.len(), 2, "both twins exist: {states:?}");
    assert!(
        states
            .iter()
            .any(|(gate, a)| gate.contains("not") && a == "active"),
        "the twin of the item's own gate keeps its own verdict: {states:?}"
    );
    assert!(
        states
            .iter()
            .any(|(gate, a)| !gate.contains("not") && a == "inactive"),
        "cfg(kani) is false by default whatever the module is: {states:?}"
    );
    let resolved = rows(
        &store,
        "MATCH (c:CallSite)-[r:Calls_CallSite_Function]->(t:Function) \
         WHERE c.callee_name ENDS WITH 'g' RETURN t.id, r.resolution_method",
    );
    assert_eq!(
        resolved.len(),
        1,
        "h's call to g is bound once: {resolved:?}"
    );
    assert!(resolved[0][0].contains("not(kani)"), "{resolved:?}");
    assert_eq!(resolved[0][1], "cfg-selected");
}

#[test]
fn an_item_keeps_its_own_verdict_under_a_module_gated_by_a_bare_option() {
    assert_contract("unix");
}

#[test]
fn an_item_keeps_its_own_verdict_under_a_module_gated_by_any_of_kani_or_unix() {
    assert_contract("any(kani, unix)");
}
