// graph_store::read_only_tests — the engine-side half of `query_graph`'s
// read-only guarantee (fleet-watch#15). Split from `graph_store/tests.rs`
// when that file crossed the §4.1 500-line cap, along the seam it already
// had: everything here exercises `execute_read_only_query`.
//
// Scope: the engine gate covers DATABASE writes (CREATE/DELETE/DROP — however
// spelled, since the classification comes from the compiled plan, not a
// keyword scan) and the query timeout. It does NOT cover
// COPY/EXPORT/IMPORT/ATTACH; those are blocked upstream by the lexical gate,
// and `engine_gate_does_not_cover_filesystem_writes` below is the measurement
// that says why the lexical layer has to exist.

use super::*;

fn store_with_one_function() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("query_read_only_test")
        .tempdir()
        .expect("create temp dir");
    let store = GraphStore::open_or_create(&dir.path().join("testdb")).expect("open_or_create");
    store.create_schema().expect("create_schema");
    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", "'fn1'"),
                ("name", "'main'"),
                ("qualified_name", "'crate::main'"),
                ("start_line", "1"),
                ("end_line", "10"),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .expect("insert_node");
    (dir, store)
}

#[test]
fn execute_read_only_query_refuses_db_writes() {
    let (_dir, store) = store_with_one_function();

    // A benign read query still returns rows under the read-only path.
    let ok = store
        .execute_read_only_query("MATCH (n:Function) RETURN n.id", 30_000)
        .expect("benign read query must succeed");
    assert_eq!(ok.rows.len(), 1, "benign read must return the one Function");

    // Valid, unambiguous writes/DDL are refused with the engine reason code
    // — proving the classification comes from the engine (is_read_only), not
    // a parse failure. Covers the mutation family the blocklist did list plus
    // forms it did not (DETACH DELETE, DROP). (cortex-16 vigilance #2.)
    for write in [
        "CREATE (:Function {id: 'evil'})",
        "MATCH (n:Function) DETACH DELETE n",
        "DROP TABLE Function",
    ] {
        let err = store
            .execute_read_only_query(write, 30_000)
            .err()
            .unwrap_or_else(|| panic!("write must be refused: {write}"));
        assert!(
            err.contains("read_only_query_required"),
            "write `{write}` must be refused as read_only_query_required, got: {err}"
        );
    }
}

#[test]
fn query_timeout_does_not_leak_onto_the_shared_connection() {
    // fleet-watch review finding 10 (regression). The previous test asserted
    // that a read completes inside a 1 ms bound — a verdict on machine speed,
    // which fails on a loaded runner — and never asserted the reset its own
    // name claimed.
    //
    // This asserts the reset instead, and its PASS condition does not depend
    // on machine speed. `set_query_timeout` is per-CONNECTION and `GraphStore`
    // shares one connection with the write tools, so a leaked 1 ms bound would
    // abort subsequent work on the same handle. The follow-up below is a bulk
    // write of 2_000 rows, which takes longer than 1 ms on any machine: it
    // succeeds whenever the reset happened, and aborts whenever it did not.
    // A slower machine only makes the leak MORE detectable.
    let (_dir, store) = store_with_one_function();

    // Whether this read finishes inside 1 ms is irrelevant and deliberately
    // unasserted — either outcome must leave the connection unbounded.
    let _ = store.execute_read_only_query("MATCH (n:Function) RETURN n.id", 1);
    // Same for the refusal path, which returns before execution.
    let _ = store.execute_read_only_query("CREATE (:Function {id: 'evil'})", 1);

    let rows: Vec<Vec<(String, String)>> = (0..2_000)
        .map(|i| {
            vec![
                ("id".to_string(), format!("'leak{i}'")),
                ("name".to_string(), "'leak'".to_string()),
                ("qualified_name".to_string(), format!("'leak::{i}'")),
                ("start_line".to_string(), "1".to_string()),
                ("end_line".to_string(), "2".to_string()),
                ("visibility".to_string(), "'pub'".to_string()),
                ("is_async".to_string(), "false".to_string()),
            ]
        })
        .collect();
    store
        .bulk_insert_nodes(NODE_FUNCTION, &rows)
        .expect("a bulk write after a bounded read must not inherit its timeout");
    assert_eq!(
        store
            .execute_query("MATCH (n:Function) WHERE n.name = 'leak' RETURN n.id")
            .expect("read back")
            .rows
            .len(),
        2_000,
        "every row of the follow-up write must have landed"
    );
}

#[test]
fn engine_gate_does_not_cover_filesystem_writes() {
    // fleet-watch review finding 8 (refutation, pinned as a test so the
    // conclusion cannot drift). The review proposed replacing the lexical
    // keyword gate with a `SystemConfig::read_only(true)` handle, on the
    // premise that such a handle refuses COPY/EXPORT/IMPORT/ATTACH "by
    // construction". It does not: both engine gates share one predicate
    // (`StatementReadWriteAnalyzer`, reached from `PreparedStatement::
    // is_read_only` and from `ClientContext::validateTransaction`), and that
    // predicate overrides `visitCopyFrom` while leaving `visitCopyTo` and
    // `visitExportDatabase` at the base visitor's no-op.
    //
    // So `COPY (..) TO 'file'` passes the engine gate and WRITES THE
    // FILESYSTEM. Deleting the lexical gate on the strength of an engine
    // read-only handle would open an arbitrary-file-write hole. This test
    // fails the day lbug closes the gap — at which point the lexical entries
    // may be reconsidered, with evidence.
    let (dir, store) = store_with_one_function();
    let out = dir.path().join("engine_gate_probe.csv");
    assert!(!out.exists());

    let cypher = format!(
        "COPY (MATCH (n:Function) RETURN n.id) TO '{}'",
        out.display()
    );
    let refusal = store.execute_read_only_query(&cypher, 30_000).err();
    assert!(
        refusal.is_none(),
        "engine gate is expected NOT to refuse COPY .. TO; if this now fails, \
         lbug has closed the gap and this test's premise must be re-derived: {refusal:?}"
    );
    assert!(
        out.exists(),
        "COPY .. TO must be shown to actually reach the filesystem — that is \
         what makes the lexical gate load-bearing"
    );
    // The other half of the argument — that the lexical gate refuses these —
    // lives in the binary crate, where the gate does:
    // `query_handlers_tests::readonly_gate_blocks_filesystem_writing_statements`.
}
