// The rows `trace_processes` writes through `ProcessBatch`: the same nodes,
// entry edges and per-participant depths the per-process writes produced
// (issue #443). The fixture is the chain main -> step_one -> step_two.

use crate::graph_store::GraphStore;
use crate::test_support::TempDirExt;
use std::collections::HashMap;

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

fn index_and_resolve(
    prefix: &str,
    file: &str,
    code: &str,
) -> (crate::test_support::TestTempDir, GraphStore) {
    let tmp = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir")
        .keep_managed();
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join(file), code).unwrap();
    let graph_dir = tmp.join("graph");
    crate::indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    crate::resolver::resolve_graph(&store).expect("resolve");
    (tmp, store)
}

/// Twenty public entries `e0..e19` all call one hub, which calls thirty leaves
/// and one method. Each process reaches its entry, the hub and the thirty
/// leaves (32 Function rows) plus the method: 640 `ParticipatesIn_Function`
/// rows, so the loop crosses the 500-row chunk boundary and flushes a full
/// chunk while later processes are still pending. Every row must still be
/// written once, with its depth, in both participant tables.
#[test]
fn trace_processes_writes_every_row_of_many_processes_across_a_chunk_boundary() {
    const ENTRIES: usize = 20;
    const LEAVES: usize = 30;
    let mut code = String::from("pub struct S;\nimpl S {\n    pub fn method() {}\n}\n");
    for e in 0..ENTRIES {
        code.push_str(&format!("pub fn e{e}() {{\n    hub();\n}}\n"));
    }
    code.push_str("fn hub() {\n    S::method();\n");
    for l in 0..LEAVES {
        code.push_str(&format!("    leaf{l}();\n"));
    }
    code.push_str("}\n");
    for l in 0..LEAVES {
        code.push_str(&format!("fn leaf{l}() {{}}\n"));
    }
    let (_tmp, store) = index_and_resolve("process_batch_many_", "lib.rs", &code);

    super::super::trace_processes(&store).expect("trace");

    let per_process = rows(
        &store,
        "MATCH (f:Function)-[r:ParticipatesIn_Function_Process]->(p:Process) \
         WHERE p.entry_kind = 'lib_entry' RETURN p.id, count(*)",
    );
    assert_eq!(per_process.len(), ENTRIES, "{per_process:?}");
    for row in &per_process {
        assert_eq!(row[1], (LEAVES + 2).to_string(), "{row:?}");
    }
    // Depths: each entry 0, the hub 1, each leaf 2.
    let depths = rows(
        &store,
        "MATCH (f:Function)-[r:ParticipatesIn_Function_Process]->(p:Process) \
         WHERE p.entry_kind = 'lib_entry' RETURN r.depth, count(*)",
    );
    let expected: Vec<Vec<String>> = [(0, ENTRIES), (1, ENTRIES), (2, ENTRIES * LEAVES)]
        .iter()
        .map(|(d, n)| vec![d.to_string(), n.to_string()])
        .collect();
    assert_eq!(depths, expected, "{depths:?}");
    let methods = rows(
        &store,
        "MATCH (m:Method {name: 'method'})-[r:ParticipatesIn_Method_Process]->(p:Process) \
         WHERE p.entry_kind = 'lib_entry' RETURN r.depth, count(*)",
    );
    assert_eq!(
        methods,
        [["2".to_string(), ENTRIES.to_string()]],
        "{methods:?}"
    );
    let entry_edges = rows(
        &store,
        "MATCH (f:Function)-[:EntryPointOf_Function_Process]->(p:Process) \
         WHERE p.entry_kind = 'lib_entry' RETURN count(*)",
    );
    assert_eq!(entry_edges, [[ENTRIES.to_string()]], "{entry_edges:?}");
}

/// The memory bound of review #561: N processes that each reach the same N
/// functions hold N^2 `ParticipatesIn` rows. Between two processes, the batch
/// must keep fewer than one chunk per table, and the rows must all be written.
#[test]
fn process_batch_keeps_less_than_one_chunk_per_table_between_processes() {
    use crate::graph_store::{BULK_BATCH_SIZE, NODE_FUNCTION};
    const N: usize = 60;
    let tmp = tempfile::Builder::new()
        .prefix("process_batch_bound_")
        .tempdir()
        .expect("tempdir")
        .keep_managed();
    let store = GraphStore::open_or_create(&tmp.join("graph")).unwrap();
    store.create_schema().unwrap();
    let ids: Vec<String> = (0..N).map(|i| format!("lib.rs::f{i}")).collect();
    let functions: Vec<Vec<(String, String)>> = ids
        .iter()
        .map(|id| {
            vec![
                ("id".to_string(), crate::graph_store::cypher_str(id)),
                ("name".to_string(), crate::graph_store::cypher_str(id)),
            ]
        })
        .collect();
    store.bulk_insert_nodes(NODE_FUNCTION, &functions).unwrap();
    let labels: HashMap<String, String> = ids
        .iter()
        .map(|id| (id.clone(), "Function".to_string()))
        .collect();
    let depths: HashMap<String, u64> = ids.iter().map(|id| (id.clone(), 1)).collect();

    let mut batch = super::ProcessBatch::default();
    let mut peak = 0;
    for id in &ids {
        let entry = super::super::EntryPoint {
            id: id.clone(),
            label: "Function".into(),
            name: id.clone(),
            qualified_name: id.clone(),
            kind: "lib_entry".into(),
            confidence: 0.6,
        };
        let process_id = format!("process::{id}");
        batch.add_process(&entry, &process_id, N as u64, 1);
        batch.add_participants(&depths, &process_id, &labels);
        batch.flush_full_chunks(&store).expect("flush");
        let (nodes, tables) = batch.pending();
        assert!(nodes < BULK_BATCH_SIZE, "{nodes} pending Process rows");
        for pending in tables {
            assert!(pending < BULK_BATCH_SIZE, "{pending} pending edge rows");
            peak = peak.max(pending);
        }
    }
    batch.write(&store).expect("write");

    assert!(peak > 0, "the bound was never exercised");
    let participants = rows(
        &store,
        "MATCH (:Function)-[r:ParticipatesIn_Function_Process]->(:Process) RETURN count(*)",
    );
    assert_eq!(participants, [[(N * N).to_string()]], "{participants:?}");
    let processes = rows(&store, "MATCH (p:Process) RETURN count(*)");
    assert_eq!(processes, [[N.to_string()]], "{processes:?}");
    let entries = rows(
        &store,
        "MATCH (:Function)-[r:EntryPointOf_Function_Process]->(:Process) RETURN count(*)",
    );
    assert_eq!(entries, [[N.to_string()]], "{entries:?}");
}
