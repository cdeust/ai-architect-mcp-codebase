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
        for (_, pending) in tables {
            assert!(pending < BULK_BATCH_SIZE, "{pending} pending edge rows");
            peak = peak.max(pending);
        }
    }
    batch.write(&store).expect("write");

    assert!(peak > 0, "the bound was never exercised");
    // Review of #570, note 2: N * N rows are not a multiple of the chunk, so
    // flushes leave remainders; each row must be written once, which a total
    // per table does not show.
    let processes: Vec<(String, String)> = ids
        .iter()
        .map(|id| (id.clone(), format!("process::{id}")))
        .collect();
    let nodes = rows(&store, "MATCH (p:Process) RETURN p.id");
    let mut expected_nodes: Vec<Vec<String>> =
        processes.iter().map(|(_, p)| vec![p.clone()]).collect();
    expected_nodes.sort();
    assert_eq!(nodes, expected_nodes, "Process nodes");
    assert_each_edge_once(&store, "EntryPointOf_Function_Process", &processes);
    let participants: Vec<(String, String)> = processes
        .iter()
        .flat_map(|(_, p)| ids.iter().map(move |f| (f.clone(), p.clone())))
        .collect();
    assert_each_edge_once(&store, "ParticipatesIn_Function_Process", &participants);
}

/// A store holding `n` functions `lib.rs::f0..`, their id -> label map, and
/// the depth map of a process that reaches all of them at depth 1.
struct Functions {
    _tmp: crate::test_support::TestTempDir,
    store: GraphStore,
    ids: Vec<String>,
    labels: HashMap<String, String>,
    depths: HashMap<String, u64>,
}

fn store_with_functions(prefix: &str, n: usize) -> Functions {
    use crate::graph_store::{cypher_str, NODE_FUNCTION};
    let tmp = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir")
        .keep_managed();
    let store = GraphStore::open_or_create(&tmp.join("graph")).unwrap();
    store.create_schema().unwrap();
    let ids: Vec<String> = (0..n).map(|i| format!("lib.rs::f{i}")).collect();
    let functions: Vec<Vec<(String, String)>> = ids
        .iter()
        .map(|id| {
            vec![
                ("id".to_string(), cypher_str(id)),
                ("name".to_string(), cypher_str(id)),
            ]
        })
        .collect();
    store.bulk_insert_nodes(NODE_FUNCTION, &functions).unwrap();
    let labels = ids
        .iter()
        .map(|id| (id.clone(), "Function".to_string()))
        .collect();
    let depths = ids.iter().map(|id| (id.clone(), 1)).collect();
    Functions {
        _tmp: tmp,
        store,
        ids,
        labels,
        depths,
    }
}

fn entry(id: &str, label: &str) -> super::super::EntryPoint {
    super::super::EntryPoint {
        id: id.to_string(),
        label: label.into(),
        name: id.to_string(),
        qualified_name: id.to_string(),
        kind: "lib_entry".into(),
        confidence: 0.6,
    }
}

/// Review of #563, note 3: the boundary of the edge trigger. The first
/// assertion requires `BULK_BATCH_SIZE` to be a multiple of 5. Each process
/// then adds `BULK_BATCH_SIZE / 5` participants, so the `ParticipatesIn`
/// table holds exactly one full chunk at every fifth process and one flush
/// must empty it: a `>=` turned into `>` keeps the chunk. After every process
/// (its `flush_full_chunks` call, which may write nothing) each edge table
/// must hold what was added modulo one chunk, the `Process` nodes only those
/// added since the last flush, and the landings are counted, `PROCESSES / 5`
/// of them. The final write must leave every row in the store, each once:
/// the `Process` nodes by id, and each `EntryPointOf` and `ParticipatesIn`
/// edge by its source and its `Process`, so a write that duplicates one row
/// and drops another in the same table fails where a total per table would
/// not (reviews of #564, #565, #566, #567, #568, #569).
#[test]
fn process_batch_flushes_an_edge_table_that_lands_exactly_on_a_chunk() {
    use crate::graph_store::BULK_BATCH_SIZE;
    const PROCESSES: usize = 12;
    assert_eq!(
        BULK_BATCH_SIZE % 5,
        0,
        "the expected landing count, PROCESSES / 5, assumes BULK_BATCH_SIZE is a multiple of 5"
    );
    let per_process = BULK_BATCH_SIZE / 5;
    let Functions {
        _tmp,
        store,
        ids,
        labels,
        depths,
    } = store_with_functions("process_batch_exact_", per_process);

    let mut batch = super::ProcessBatch::default();
    let mut landings = 0;
    for (k, id) in ids.iter().cycle().take(PROCESSES).enumerate() {
        let process_id = format!("process::{k}::{id}");
        batch.add_process(&entry(id, "Function"), &process_id, per_process as u64, 1);
        batch.add_participants(&depths, &process_id, &labels);
        let before = participates_pending(&batch);
        batch.flush_full_chunks(&store).expect("flush");
        if before == BULK_BATCH_SIZE {
            assert_eq!(
                participates_pending(&batch),
                0,
                "one flush empties the table"
            );
            landings += 1;
        }
        let done = k + 1;
        let (nodes, tables) = batch.pending();
        assert_eq!(nodes, done % 5, "Process nodes after process {done}");
        let expected = vec![
            (
                "EntryPointOf_Function_Process".to_string(),
                done % BULK_BATCH_SIZE,
            ),
            (
                "ParticipatesIn_Function_Process".to_string(),
                (done * per_process) % BULK_BATCH_SIZE,
            ),
        ];
        assert_eq!(tables, expected, "after process {done}");
    }
    assert_eq!(landings, PROCESSES / 5, "the table landed on a full chunk");
    batch.write(&store).expect("write");

    let processes: Vec<(String, String)> = ids
        .iter()
        .cycle()
        .take(PROCESSES)
        .enumerate()
        .map(|(k, id)| (id.clone(), format!("process::{k}::{id}")))
        .collect();
    let nodes = rows(&store, "MATCH (p:Process) RETURN p.id");
    let mut expected_nodes: Vec<Vec<String>> =
        processes.iter().map(|(_, p)| vec![p.clone()]).collect();
    expected_nodes.sort();
    assert_eq!(nodes, expected_nodes, "Process nodes");
    assert_each_edge_once(&store, "EntryPointOf_Function_Process", &processes);
    let participants: Vec<(String, String)> = processes
        .iter()
        .flat_map(|(_, p)| ids.iter().map(move |f| (f.clone(), p.clone())))
        .collect();
    assert_each_edge_once(&store, "ParticipatesIn_Function_Process", &participants);
}

/// Review of #569, note 3, and review of #570, note 1: `EntryPointOf`
/// through an early flush that only the edge trigger fires. Every process
/// adds one `Process` node and one `EntryPointOf` row and no participant,
/// except process `EXTRA`, which reaches every function: its
/// `ParticipatesIn` table then holds more than one chunk and its flush writes
/// the `EXTRA` pending nodes and that chunk, but keeps the `EXTRA` entry
/// edges and the `EXTRA` participants past the chunk. From there the entry
/// edges lead the nodes by `EXTRA`, so at the `BULK_BATCH_SIZE`-th process
/// `EntryPointOf_Function_Process` holds a full chunk while the nodes hold
/// `EXTRA` fewer: the node trigger alone does not fire, and the flush must
/// still write the pending nodes, then that chunk, and keep nothing of
/// either. After every process the pending rows are checked; after the flush
/// that lands, the store holds each entry edge of the first chunk once, and
/// after the final write every row once, including the participants that
/// the first flush left as a remainder (review of #570, note 2).
#[test]
fn process_batch_flushes_entry_edges_once_they_fill_a_chunk() {
    use crate::graph_store::BULK_BATCH_SIZE;
    const REL: &str = "EntryPointOf_Function_Process";
    const PARTICIPATES: &str = "ParticipatesIn_Function_Process";
    const EXTRA: usize = 3;
    let Functions {
        _tmp,
        store,
        ids,
        labels,
        depths,
    } = store_with_functions("process_batch_entries_", BULK_BATCH_SIZE + EXTRA);
    let processes: Vec<(String, String)> = ids
        .iter()
        .map(|id| (id.clone(), format!("process::{id}")))
        .collect();

    let mut batch = super::ProcessBatch::default();
    for (k, (id, process_id)) in processes.iter().enumerate() {
        let done = k + 1;
        batch.add_process(&entry(id, "Function"), process_id, 0, 0);
        if done == EXTRA {
            batch.add_participants(&depths, process_id, &labels);
        }
        if done == BULK_BATCH_SIZE {
            let (nodes, tables) = batch.pending();
            assert_eq!(
                (nodes, tables[0].clone()),
                (BULK_BATCH_SIZE - EXTRA, (REL.to_string(), BULK_BATCH_SIZE)),
                "only the edge trigger fires at process {done}"
            );
        }
        batch.flush_full_chunks(&store).expect("flush");
        let pending_nodes = match done {
            d if d < EXTRA => d,
            d if d < BULK_BATCH_SIZE => d - EXTRA,
            d => d - BULK_BATCH_SIZE,
        };
        let mut expected = vec![(REL.to_string(), done % BULK_BATCH_SIZE)];
        if done >= EXTRA {
            expected.push((PARTICIPATES.to_string(), EXTRA));
        }
        assert_eq!(
            batch.pending(),
            (pending_nodes, expected),
            "after process {done}"
        );
        if done == BULK_BATCH_SIZE {
            assert_each_edge_once(&store, REL, &processes[..BULK_BATCH_SIZE]);
        }
    }
    batch.write(&store).expect("write");

    assert_each_edge_once(&store, REL, &processes);
    let process_id = &processes[EXTRA - 1].1;
    let participants: Vec<(String, String)> = ids
        .iter()
        .map(|f| (f.clone(), process_id.clone()))
        .collect();
    assert_each_edge_once(&store, PARTICIPATES, &participants);
}

/// The store holds exactly the `rel` edges `expected` names by
/// (source id, `Process` id), each once.
fn assert_each_edge_once(store: &GraphStore, rel: &str, expected: &[(String, String)]) {
    let written = rows(
        store,
        &format!("MATCH (s)-[:{rel}]->(p:Process) RETURN s.id, p.id, count(*)"),
    );
    let mut want: Vec<Vec<String>> = expected
        .iter()
        .map(|(s, p)| vec![s.clone(), p.clone(), "1".to_string()])
        .collect();
    want.sort();
    assert_eq!(written, want, "{rel}");
}

/// Review of #563, note 3: the node trigger. Entries whose label has no
/// `EntryPointOf` table and no participants leave every edge table empty, so
/// only the nodes can fill a chunk; the batch must then write them.
#[test]
fn process_batch_writes_the_nodes_once_they_fill_a_chunk_without_edges() {
    use crate::graph_store::BULK_BATCH_SIZE;
    let extra = 3;
    let Functions { _tmp, store, .. } = store_with_functions("process_batch_nodes_", 0);

    let mut batch = super::ProcessBatch::default();
    for k in 0..BULK_BATCH_SIZE + extra {
        let id = format!("lib.rs::C{k}");
        batch.add_process(&entry(&id, "Class"), &format!("process::{id}"), 0, 0);
        batch.flush_full_chunks(&store).expect("flush");
        let (nodes, tables) = batch.pending();
        assert!(tables.is_empty(), "{tables:?}");
        assert_eq!(nodes, (k + 1) % BULK_BATCH_SIZE, "after process {}", k + 1);
    }
    let written = rows(&store, "MATCH (p:Process) RETURN count(*)");
    assert_eq!(written, [[BULK_BATCH_SIZE.to_string()]], "{written:?}");
    batch.write(&store).expect("write");

    let processes = rows(&store, "MATCH (p:Process) RETURN count(*)");
    assert_eq!(
        processes,
        [[(BULK_BATCH_SIZE + extra).to_string()]],
        "{processes:?}"
    );
}

/// Pending `ParticipatesIn_Function_Process` rows of `batch`.
fn participates_pending(batch: &super::ProcessBatch) -> usize {
    batch
        .pending()
        .1
        .into_iter()
        .find(|(table, _)| table == "ParticipatesIn_Function_Process")
        .map_or(0, |(_, n)| n)
}
