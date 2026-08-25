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

/// Mechanical re-audit of the filesystem/database-movement family, 2026-08-25.
///
/// `engine_gate_does_not_cover_filesystem_writes` above proves the gap for one
/// statement by executing it. This proves it for the whole family without
/// needing each to succeed: it asserts the engine never refuses any of them ON
/// READ-ONLY GROUNDS. A statement may still fail for its own reasons (a missing
/// file, an absent database) — what must not appear is
/// `read_only_query_required`, because that would mean the engine had started
/// classifying it as a write and the lexical entry could be reconsidered.
///
/// The list is derived from lbug 0.19.1's own headers rather than from prose:
/// `StatementReadWriteAnalyzer` overrides `visitCopyFrom` -> readOnly = false
/// and leaves `visitCopyTo`, `visitExportDatabase`, `visitImportDatabase`,
/// `visitAttachDatabase`, `visitDetachDatabase` and `visitUseDatabase` at the
/// base visitor's no-op (parsed_statement_visitor.h:51, 57-61). Six, not the
/// four this project's comment listed — DETACH and USE were in neither gate
/// until this pass added them.
///
/// This test fails the day lbug closes any of these gaps, which is when the
/// corresponding lexical entry may be revisited WITH EVIDENCE.
#[test]
fn engine_classifies_every_filesystem_statement_as_read_only() {
    let (dir, store) = store_with_one_function();
    let scratch = dir.path().join("scratch");
    let probes = [
        format!(
            "COPY (MATCH (n:Function) RETURN n.id) TO '{}'",
            dir.path().join("probe.csv").display()
        ),
        format!("EXPORT DATABASE '{}'", scratch.display()),
        format!("IMPORT DATABASE '{}'", scratch.display()),
        format!("ATTACH '{}' AS probe_db (dbtype lbug)", scratch.display()),
        "DETACH probe_db".to_string(),
        "USE probe_db".to_string(),
    ];

    for cypher in &probes {
        if let Err(e) = store.execute_read_only_query(cypher, 30_000) {
            assert!(
                !e.contains("read_only_query_required"),
                "lbug now REFUSES `{cypher}` as a write. The gap this project's \
                 lexical gate exists to cover has closed for that statement; \
                 re-derive the denylist entry with this as evidence. Error: {e}"
            );
        }
    }
}

/// The statement cache must not be BORROWED while a prepared statement is in
/// use. Holding a `RefMut` across `conn.execute` is a double-borrow panic
/// waiting for the first caller that reaches this cache from inside an execute
/// or a drain — and this store is cached process-wide, so that panic would take
/// the server down rather than one request.
///
/// No red-then-green pair exists for this one, and that is the finding rather
/// than an omission: the panic needs a caller that does not exist yet, which is
/// exactly why the review graded it "one refactor away". What IS testable is
/// the invariant that makes the panic impossible — the cache is free while the
/// statement is out — so that is what this pins. Under the previous shape the
/// assertion could not even be written: the statement was a reference INTO the
/// map, so its existence and the guard's were the same fact.
#[test]
fn a_statement_in_use_does_not_hold_the_cache_borrowed() {
    let (_dir, store) = store_with_one_function();
    let cypher = "MATCH (n:Function) RETURN n.id";

    let stmt = store.take_cached_stmt(cypher).expect("prepare");
    assert!(
        store.stmt_cache.try_borrow_mut().is_ok(),
        "the cache must be borrowable while a statement is checked out"
    );
    store.return_cached_stmt(cypher, stmt);
}

/// The statement goes back on BOTH paths, so the cache keeps its
/// plan-once/execute-many property. A dropped statement would silently turn
/// caching off for that cypher and nothing would report it.
///
/// The failing case is asserted on ITS OWN cache entry. An earlier version of
/// this test only re-checked an unrelated, already-cached statement afterwards,
/// so a regression that dropped `return_cached_stmt` from the `Err` arm alone
/// would still have passed — the sole guard for the invariant, not testing what
/// it claimed.
#[test]
fn a_prepared_statement_is_returned_to_the_cache_after_use() {
    let (_dir, store) = store_with_one_function();
    let ok_cypher = "MATCH (n:Function) RETURN n.id";

    assert!(!store.stmt_cache.borrow().contains_key(ok_cypher));
    store
        .query_prepared_params(ok_cypher, Vec::new())
        .expect("first call prepares");
    assert!(
        store
            .stmt_cache
            .borrow()
            .get(ok_cypher)
            .is_some_and(Option::is_some),
        "a successful call must leave its plan behind, IN the slot"
    );

    // A statement that PREPARES but fails at execute: the parameter it binds is
    // never supplied. The plan is still valid, so its own entry must come back.
    let failing = "MATCH (n:Function) WHERE n.id = $never_bound RETURN n.id";
    assert!(
        store.query_prepared_params(failing, Vec::new()).is_err(),
        "fixture precondition: this must fail at execute, not at prepare"
    );
    assert!(
        store
            .stmt_cache
            .borrow()
            .get(failing)
            .is_some_and(Option::is_some),
        "the FAILING statement's own plan must be back IN its slot — a failed \
         execute does not invalidate the compiled plan"
    );
}

/// Round-6 finding 5. Returning the statement used to re-insert the KEY as
/// well, allocating a fresh `String` on every call and regressing the
/// bulk-insert loop this cache exists for — `run_prepared_params`' own contract
/// is "one plan, many executes" — from zero allocations after the first chunk
/// to one alloc/dealloc pair per chunk.
///
/// The entry is now left in place and only its VALUE moves, so the observable
/// invariant is: repeated calls never change the cache's key set, and the slot
/// is refilled each time.
#[test]
fn repeated_calls_reuse_the_cache_entry_without_replacing_the_key() {
    let (_dir, store) = store_with_one_function();
    let cypher = "MATCH (n:Function) RETURN n.id";

    store
        .query_prepared_params(cypher, Vec::new())
        .expect("1st");
    let keys_after_first: Vec<String> = store.stmt_cache.borrow().keys().cloned().collect();

    for _ in 0..5 {
        store
            .query_prepared_params(cypher, Vec::new())
            .expect("nth");
        assert!(
            store
                .stmt_cache
                .borrow()
                .get(cypher)
                .is_some_and(Option::is_some),
            "the plan must be back in its slot after every call"
        );
    }

    let keys_after_many: Vec<String> = store.stmt_cache.borrow().keys().cloned().collect();
    assert_eq!(
        keys_after_first, keys_after_many,
        "the key set must not change — a re-inserted key is a fresh allocation"
    );
}
