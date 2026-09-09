// rust_receiver_static_resolution — end-to-end regression for issue #283
// lot 4: static binding of Rust `self.<m>` / `Self::<m>` receiver calls
// (paliers 1-2). Palier 3 (local receivers, lot 6) has its own integration
// suite, `tests/rust_local_receiver_static_resolution.rs` — kept separate so
// this file stays under the §4.1 500-line cap rather than growing past it.
//
// Drives the REAL indexer + resolver pipeline (no LSP) to prove:
//   1. `self.<m>` on a Method caller binds to the enclosing impl's method
//      (palier 1, exact key) with `Evidence::ReceiverBound` confidence and
//      the `receiver-type` resolution_method label.
//   2. `Self::<m>` binds the same way (associated-function form).
//   3. `self.<missing>()` (no such method) stays `UnresolvedRef{reason:
//      "no target found"}` — no edge fabricated.
//   4. Two homonymous types (`a.rs::TaskSet::m`, `b.rs::TaskSet::m`): a
//      caller inside `a.rs`'s own impl resolves via palier 1 (exact key,
//      same file); a caller inside a THIRD file's `impl TaskSet` with no
//      local `m` falls to palier 2 and lands `Ambiguous` — no edge, no
//      guess.
//   5. Receiver shapes the classifier refuses to guess through
//      (`self.tasks.get`, `sets[0].response_of`, `x.trim().len`) are
//      unchanged: still `NotFound`, never a false edge.
//   6. Python `self.m()` is unaffected — the gate is `language == "rust"`
//      only.
//
// source: tasks/plan-issues-282-283-284.md §2.4 (lot 4 test list).

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;
mod common;
use common::TempDirExt;

/// Writes `files` into a fresh tempdir, indexes and resolves it, and returns
/// the open graph plus the resolution result and the tempdir guard (the
/// on-disk graph outlives the queries; RAII cleans it up on drop — mirrors
/// `tests/kotlin_ambiguous_calls.rs::index_and_resolve`).
fn index_and_resolve(
    tag: &str,
    files: &[(&str, &str)],
) -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let root = tempfile::Builder::new()
        .prefix(&format!("rust_receiver_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = root.join("src");
    for (rel, body) in files {
        let p = src.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create fixture dir");
        }
        fs::write(&p, body).expect("write fixture file");
    }
    let graph_dir = root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let res = resolver::resolve_graph(&store).expect("resolve_graph");
    (store, res, root)
}

/// Every `Calls_Method_Method` edge whose target qualified_name contains
/// `callee_contains`, as (caller_id, target_qn, confidence, resolution_method).
fn calls_edges(store: &GraphStore, callee_contains: &str) -> Vec<Vec<String>> {
    let qr = store
        .execute_query(
            "MATCH (a:Method)-[r:Calls_Method_Method]->(b:Method) \
             RETURN a.id, b.qualified_name, r.confidence, r.resolution_method",
        )
        .expect("query Calls_Method_Method");
    qr.rows
        .into_iter()
        .filter(|row| row.get(1).is_some_and(|qn| qn.contains(callee_contains)))
        .collect()
}

/// `CallSite.is_resolved` for the single CallSite whose `callee_name`
/// equals `callee_name` exactly.
fn callsite_is_resolved(store: &GraphStore, callee_name: &str) -> bool {
    let qr = store
        .execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.callee_name = '{callee_name}' \
             RETURN cs.is_resolved"
        ))
        .expect("query CallSite");
    assert_eq!(
        qr.rows.len(),
        1,
        "expected exactly one CallSite with callee_name '{callee_name}', got {:?}",
        qr.rows
    );
    qr.rows[0][0] == "true"
}

// ---------------------------------------------------------------------------
// Test 1 — palier 1: self.<m> exact key.
// ---------------------------------------------------------------------------

#[test]
fn self_value_receiver_binds_to_enclosing_impl_method() {
    let src = "pub struct TaskSet;\n\
               \n\
               impl TaskSet {\n\
               \x20\x20\x20\x20pub fn response_of(&self) -> i32 { 1 }\n\
               \n\
               \x20\x20\x20\x20pub fn total(&self) -> i32 {\n\
               \x20\x20\x20\x20\x20\x20\x20\x20self.response_of()\n\
               \x20\x20\x20\x20}\n\
               }\n";
    let (store, res, _root) = index_and_resolve("self_value", &[("lib.rs", src)]);

    let edges = calls_edges(&store, "response_of");
    assert_eq!(
        edges.len(),
        1,
        "expected exactly one Calls edge into response_of; unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
    let confidence: f64 = edges[0][2].parse().unwrap_or(0.0);
    assert!(
        (confidence - 0.93).abs() < 1e-9,
        "expected ReceiverBound confidence (0.93), got {confidence}"
    );
    assert_eq!(edges[0][3], "receiver-type");
    assert!(
        callsite_is_resolved(&store, "self.response_of"),
        "CallSite.is_resolved must flip to true for a receiver-bound edge"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — palier 1: Self::<m> associated-function form.
// ---------------------------------------------------------------------------

#[test]
fn self_type_receiver_binds_associated_function() {
    let src = "pub struct TaskSet;\n\
               \n\
               impl TaskSet {\n\
               \x20\x20\x20\x20pub fn new() -> TaskSet { TaskSet }\n\
               \n\
               \x20\x20\x20\x20pub fn make() -> TaskSet {\n\
               \x20\x20\x20\x20\x20\x20\x20\x20Self::new()\n\
               \x20\x20\x20\x20}\n\
               }\n";
    let (store, res, _root) = index_and_resolve("self_type", &[("lib.rs", src)]);

    let edges = calls_edges(&store, "TaskSet::new");
    assert_eq!(
        edges.len(),
        1,
        "expected exactly one Calls edge into TaskSet::new; unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
    assert_eq!(edges[0][3], "receiver-type");
}

// ---------------------------------------------------------------------------
// Test 3 — self.<missing>(): no such method, must stay unresolved.
// ---------------------------------------------------------------------------

#[test]
fn self_receiver_to_missing_method_is_not_found_never_fabricates_an_edge() {
    let src = "pub struct TaskSet;\n\
               \n\
               impl TaskSet {\n\
               \x20\x20\x20\x20pub fn total(&self) -> i32 {\n\
               \x20\x20\x20\x20\x20\x20\x20\x20self.missing()\n\
               \x20\x20\x20\x20}\n\
               }\n";
    let (store, res, _root) = index_and_resolve("missing", &[("lib.rs", src)]);

    let edges = calls_edges(&store, "missing");
    assert!(
        edges.is_empty(),
        "a receiver call to a non-existent method must never produce an edge: {edges:?}"
    );
    let unresolved = res
        .unresolved
        .iter()
        .find(|u| u.kind == "Calls" && u.target_text == "self.missing");
    assert!(
        unresolved.is_some(),
        "expected an unresolved Calls ref for 'self.missing', got: {:?}",
        res.unresolved
            .iter()
            .map(|u| (&u.kind, &u.target_text, &u.reason))
            .collect::<Vec<_>>()
    );
    assert_eq!(unresolved.unwrap().reason, "no target found");
    assert!(!callsite_is_resolved(&store, "self.missing"));
}

// ---------------------------------------------------------------------------
// Test 4 — homonymous types: palier 1 picks the caller's own file; a
// caller with no local candidate and 2+ homonymous methods elsewhere is
// Ambiguous, never guessed.
// ---------------------------------------------------------------------------

/// a.rs defines `m` AND calls it locally; b.rs defines a homonymous `m`
/// with no local caller; c.rs's `impl TaskSet` has NO local `m` at all —
/// the shared fixture for both halves of this scenario (palier 1 same-file
/// hit, palier 2 ambiguity).
const HOMONYM_A: &str = "pub struct TaskSet;\n\
    \n\
    impl TaskSet {\n\
    \x20\x20\x20\x20pub fn m(&self) -> i32 { 1 }\n\
    \n\
    \x20\x20\x20\x20pub fn caller(&self) -> i32 {\n\
    \x20\x20\x20\x20\x20\x20\x20\x20self.m()\n\
    \x20\x20\x20\x20}\n\
    }\n";
const HOMONYM_B: &str = "pub struct TaskSet;\n\
    \n\
    impl TaskSet {\n\
    \x20\x20\x20\x20pub fn m(&self) -> i32 { 2 }\n\
    }\n";
const HOMONYM_C: &str = "pub struct TaskSet;\n\
    \n\
    impl TaskSet {\n\
    \x20\x20\x20\x20pub fn caller(&self) -> i32 {\n\
    \x20\x20\x20\x20\x20\x20\x20\x20self.m()\n\
    \x20\x20\x20\x20}\n\
    }\n";

#[test]
fn homonymous_types_palier1_resolves_the_callers_own_file() {
    let (store, res, _root) = index_and_resolve(
        "homonyms_p1",
        &[
            ("a.rs", HOMONYM_A),
            ("b.rs", HOMONYM_B),
            ("c.rs", HOMONYM_C),
        ],
    );

    let qr = store
        .execute_query(
            "MATCH (caller:Method)-[r:Calls_Method_Method]->(target:Method) \
             WHERE caller.qualified_name = 'a.rs::TaskSet::caller' \
             RETURN target.qualified_name, r.resolution_method",
        )
        .expect("query a.rs caller edges");
    assert_eq!(
        qr.rows.len(),
        1,
        "a.rs::TaskSet::caller must resolve to exactly one target; unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
    assert_eq!(qr.rows[0][0], "a.rs::TaskSet::m");
    assert_eq!(qr.rows[0][1], "receiver-type");
}

#[test]
fn homonymous_types_palier2_stays_ambiguous_with_no_local_candidate() {
    let (store, res, _root) = index_and_resolve(
        "homonyms_p2",
        &[
            ("a.rs", HOMONYM_A),
            ("b.rs", HOMONYM_B),
            ("c.rs", HOMONYM_C),
        ],
    );

    // c.rs's caller has no local m and two homonymous candidates: no edge.
    let qr = store
        .execute_query(
            "MATCH (caller:Method)-[r:Calls_Method_Method]->(target:Method) \
             WHERE caller.qualified_name = 'c.rs::TaskSet::caller' \
             RETURN target.qualified_name",
        )
        .expect("query c.rs caller edges");
    assert!(
        qr.rows.is_empty(),
        "c.rs::TaskSet::caller must NOT resolve — two homonymous candidates, no evidence \
         discriminates them: {:?}",
        qr.rows
    );
    let ambiguous = res.unresolved.iter().find(|u| {
        u.kind == "Calls"
            && u.from_id.starts_with("c.rs::TaskSet::caller")
            && u.target_text == "self.m"
    });
    assert!(
        ambiguous.is_some(),
        "expected an unresolved (ambiguous) Calls ref from c.rs::TaskSet::caller, got: {:?}",
        res.unresolved
            .iter()
            .map(|u| (&u.from_id, &u.target_text, &u.reason))
            .collect::<Vec<_>>()
    );
    assert!(
        ambiguous.unwrap().reason.starts_with("ambiguous ("),
        "expected reason to start with 'ambiguous (', got '{}'",
        ambiguous.unwrap().reason
    );
}

// ---------------------------------------------------------------------------
// Test 5 — receiver shapes the classifier refuses to guess through stay
// exactly as unresolved as before this lot (no false edge, no crash).
// ---------------------------------------------------------------------------

#[test]
fn unclassifiable_receiver_shapes_stay_not_found() {
    let src = "use std::collections::HashMap;\n\
               \n\
               pub struct TaskSet { pub tasks: HashMap<String, i32> }\n\
               \n\
               impl TaskSet {\n\
               \x20\x20\x20\x20pub fn response_of(&self, i: i32) -> i32 { i }\n\
               \n\
               \x20\x20\x20\x20pub fn chained(&self) -> Option<&i32> {\n\
               \x20\x20\x20\x20\x20\x20\x20\x20self.tasks.get(\"x\")\n\
               \x20\x20\x20\x20}\n\
               }\n\
               \n\
               pub fn total(sets: &[TaskSet], i: i32) -> i32 {\n\
               \x20\x20\x20\x20sets[0].response_of(i)\n\
               }\n\
               \n\
               pub fn len_of(x: &str) -> usize {\n\
               \x20\x20\x20\x20x.trim().len()\n\
               }\n";
    let (_store, res, _root) = index_and_resolve("unclassifiable", &[("lib.rs", src)]);

    for target in ["self.tasks.get", "sets[0].response_of", "x.trim().len"] {
        let hit = res
            .unresolved
            .iter()
            .find(|u| u.kind == "Calls" && u.target_text == target);
        assert!(
            hit.is_some(),
            "expected '{target}' to stay unresolved (unchanged pre-lot-4 behavior); \
             unresolved={:?}",
            res.unresolved
                .iter()
                .map(|u| (&u.target_text, &u.reason))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            hit.unwrap().reason,
            "no target found",
            "'{target}' must fall through to the pre-existing by-name path, not a new outcome"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 6 — Python self.m() is unaffected: the gate is language == "rust".
// ---------------------------------------------------------------------------

#[test]
fn python_self_receiver_path_is_unaffected() {
    let src = "class TaskSet:\n\
               \x20\x20\x20\x20def response_of(self):\n\
               \x20\x20\x20\x20\x20\x20\x20\x20return 1\n\
               \n\
               \x20\x20\x20\x20def total(self):\n\
               \x20\x20\x20\x20\x20\x20\x20\x20return self.response_of()\n";
    let (store, res, _root) = index_and_resolve("python", &[("task_set.py", src)]);

    // Whatever Python's own (pre-existing, unmodified-by-this-lot) call
    // resolution does for `self.response_of()` — this lot's Rust-only gate
    // must not have touched it. The classified proof is structural, not
    // behavioral: no CallSite carries `receiver-type` for a Python caller.
    let qr = store
        .execute_query(
            "MATCH ()-[r]->() WHERE r.resolution_method = 'receiver-type' RETURN count(*)",
        )
        .expect("query for any receiver-type edge");
    assert_eq!(
        qr.rows[0][0], "0",
        "the Rust-only receiver palier must never fire for a Python caller"
    );
    eprintln!(
        "python_self_receiver_path_is_unaffected: unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
}
