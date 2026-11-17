// The rows `trace_processes` writes through `ProcessBatch`: the same nodes,
// entry edges and per-participant depths the per-process writes produced
// (issue #443). The fixture is the chain main -> step_one -> step_two.

use crate::graph_store::GraphStore;
use crate::test_support::TempDirExt;

fn rows(store: &GraphStore, cypher: &str) -> Vec<Vec<String>> {
    let mut rows = store.execute_query(cypher).expect("query").rows;
    rows.sort();
    rows
}

#[test]
fn trace_processes_writes_one_node_its_entry_edge_and_each_participant_depth() {
    let tmp = tempfile::Builder::new()
        .prefix("process_batch_")
        .tempdir()
        .expect("tempdir")
        .keep_managed();
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("main.rs"),
        "fn main() {\n    step_one();\n}\n\
         fn step_one() {\n    step_two();\n}\n\
         fn step_two() {}\n",
    )
    .unwrap();
    let graph_dir = tmp.join("graph");
    crate::indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    crate::resolver::resolve_graph(&store).expect("resolve");

    let count = super::super::trace_processes(&store).expect("trace");

    let main_id = rows(&store, "MATCH (f:Function {name: 'main'}) RETURN f.id")[0][0].clone();
    let process = rows(
        &store,
        "MATCH (p:Process) WHERE p.entry_kind = 'main' RETURN p.name, p.entry_point_id, \
         p.entry_confidence, p.depth, p.symbol_count",
    );
    assert_eq!(
        count,
        rows(&store, "MATCH (p:Process) RETURN p.id").len() as u64
    );
    assert_eq!(
        process,
        [[
            "process::main.rs::main".to_string(),
            main_id.clone(),
            "1".to_string(),
            "2".to_string(),
            "3".to_string(),
        ]],
        "{process:?}"
    );
    let entry = rows(
        &store,
        "MATCH (f:Function)-[r:EntryPointOf_Function_Process]->\
         (p:Process {id: 'process::main.rs::main'}) RETURN f.id, r.confidence",
    );
    assert_eq!(entry, [[main_id, "1".to_string()]], "{entry:?}");
    let depths = rows(
        &store,
        "MATCH (f:Function)-[r:ParticipatesIn_Function_Process]->\
         (p:Process {id: 'process::main.rs::main'}) RETURN f.name, r.depth",
    );
    let expected: Vec<Vec<String>> = [("main", "0"), ("step_one", "1"), ("step_two", "2")]
        .iter()
        .map(|(n, d)| vec![n.to_string(), d.to_string()])
        .collect();
    assert_eq!(depths, expected, "{depths:?}");
}
