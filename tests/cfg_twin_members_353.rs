//! Issue #353, review round: twin methods (in one impl, across two impls, called
//! through a receiver or a path), twin constants under `cfg(kani)`, and the
//! outgoing edges of twin bodies. No call resolves to a twin; a twin still calls.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::fs;

fn graph_of(text: &str) -> (tempfile::TempDir, GraphStore) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("lib.rs"), text).unwrap();
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&source, &graph).expect("index");
    let store = GraphStore::open_or_create(&graph).expect("open");
    resolver::resolve_graph(&store).expect("resolve");
    (tmp, store)
}

/// Every non-structural relationship into a node of `label` named `name`:
/// (table, confidence). `Defines`, `HasMethod`, `HasField` are AST facts.
fn edges_into(store: &GraphStore, label: &str, name: &str) -> Vec<Vec<String>> {
    store
        .execute_query(&format!(
            "MATCH ()-[r]->(n:{label}) WHERE n.name = '{name}' RETURN label(r), r.confidence"
        ))
        .unwrap()
        .rows
        .into_iter()
        .filter(|r| {
            !r[0].starts_with("Defines_")
                && !r[0].starts_with("HasMethod_")
                && !r[0].starts_with("HasField_")
        })
        .collect()
}

const CALLERS: &str =
    "fn by_receiver(s: &S) {\n    s.m();\n}\nfn by_path(s: &S) {\n    S::m(s);\n}\n";
const ONE_IMPL_TWINS: &str = "struct S;\nimpl S {\n    #[cfg(unix)]\n    fn m(&self) {}\n    #[cfg(not(unix))]\n    fn m(&self) {}\n}\n";
const TWO_IMPL_TWINS: &str = "struct S;\n#[cfg(unix)]\nimpl S {\n    fn m(&self) {}\n}\n#[cfg(not(unix))]\nimpl S {\n    fn m(&self) {}\n}\n";
const ONE_METHOD: &str = "struct S;\nimpl S {\n    fn m(&self) {}\n}\n";

fn method_ids(store: &GraphStore) -> Vec<String> {
    let mut ids: Vec<String> = store
        .execute_query("MATCH (m:Method) WHERE m.name = 'm' RETURN m.id")
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    ids.sort();
    ids
}

#[test]
fn twin_methods_of_one_impl_get_no_call_edge_by_receiver_or_path() {
    let (_t, store) = graph_of(&format!("{ONE_IMPL_TWINS}{CALLERS}"));
    assert_eq!(
        method_ids(&store),
        ["lib.rs::S::m#cfg(not(unix))", "lib.rs::S::m#cfg(unix)"]
    );
    let edges = edges_into(&store, "Method", "m");
    assert!(edges.is_empty(), "a twin method was chosen: {edges:?}");
    let (_c, control) = graph_of(&format!("{ONE_METHOD}{CALLERS}"));
    assert!(
        !edges_into(&control, "Method", "m").is_empty(),
        "control: a lone method is called"
    );
}

#[test]
fn twin_methods_of_two_impls_get_no_call_edge_by_receiver_or_path() {
    let (_t, store) = graph_of(&format!("{TWO_IMPL_TWINS}{CALLERS}"));
    assert_eq!(
        method_ids(&store),
        ["lib.rs::S::m#cfg(not(unix))", "lib.rs::S::m#cfg(unix)"]
    );
    let edges = edges_into(&store, "Method", "m");
    assert!(edges.is_empty(), "a twin method was chosen: {edges:?}");
}

#[test]
fn twin_kani_constants_are_two_nodes_and_neither_is_chosen() {
    let text = "#[cfg(kani)]\npub const MAX: usize = 4;\n#[cfg(not(kani))]\npub const MAX: usize = 64;\npub fn f() -> usize {\n    MAX\n}\n";
    let (_t, store) = graph_of(text);
    let mut ids: Vec<String> = store
        .execute_query("MATCH (c:Constant) WHERE c.name = 'MAX' RETURN c.id")
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    ids.sort();
    assert_eq!(ids, ["lib.rs::MAX#cfg(kani)", "lib.rs::MAX#cfg(not(kani))"]);
    let edges = edges_into(&store, "Constant", "MAX");
    assert!(edges.is_empty(), "a twin constant was chosen: {edges:?}");
}

/// A twin still calls: each twin body's call to a plain helper is an edge from
/// THAT twin's id, so the twins do not lose their outgoing edges.
#[test]
fn each_twin_keeps_its_outgoing_call_edge() {
    let text = "pub fn helper() {}\n#[cfg(unix)]\npub fn pick() {\n    helper();\n}\n#[cfg(not(unix))]\npub fn pick() {\n    helper();\n}\n";
    let (_t, store) = graph_of(text);
    let mut callers: Vec<String> = store
        .execute_query(
            "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) WHERE b.name = 'helper' RETURN a.id",
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    callers.sort();
    assert_eq!(
        callers,
        ["lib.rs::pick#cfg(not(unix))", "lib.rs::pick#cfg(unix)"]
    );
}
