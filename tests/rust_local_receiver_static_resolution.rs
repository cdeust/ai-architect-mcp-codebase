// rust_local_receiver_static_resolution — end-to-end regression for issue
// #283 lot 6: static binding of Rust local-variable receiver calls (palier
// 3), via the parser-attached `CallSite.receiver_hint`.
//
// Split from `tests/rust_receiver_static_resolution.rs` (lot 4's paliers 1-2
// suite) to keep both files under the §4.1 500-line cap.
//
// Drives the REAL indexer + resolver pipeline (no LSP) to prove:
//   1. `let s = T::new(); s.m()` resolves via palier 3
//      (`Evidence::ReceiverLocalBinding`, confidence 0.87,
//      `receiver-local-binding` resolution_method label).
//   2. A typed reference parameter (`fn f(s: &T) { s.m() }`) resolves the
//      same way.
//   3. Two homonymous types + a local receiver: same-file preference
//      resolves the caller's own file's candidate; a caller in neither
//      file is `Ambiguous`, no edge.
//   4. A dy-wcet-shaped fixture (`src/lib.rs` + `tests/x.rs` + `kani/h.rs`,
//      each with its own once-bound `let s = TaskSet::new()`) resolves
//      every out-of-file caller statically, with zero edges into any other
//      symbol (no false callers).
//
// source: tasks/plan-issues-282-283-284.md §2.4 (lot 6 test list).

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;
mod common;
use common::TempDirExt;

/// Writes `files` into a fresh tempdir, indexes and resolves it, and returns
/// the open graph plus the resolution result and the tempdir guard — mirrors
/// `rust_receiver_static_resolution.rs::index_and_resolve`.
fn index_and_resolve(
    tag: &str,
    files: &[(&str, &str)],
) -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    let root = tempfile::Builder::new()
        .prefix(&format!("rust_local_receiver_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    for (rel, body) in files {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create fixture dir");
        }
        fs::write(&p, body).expect("write fixture file");
    }
    let graph_dir = root.join("graph");
    indexer::index_codebase(&root, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let res = resolver::resolve_graph(&store).expect("resolve_graph");
    (store, res, root)
}

/// Every `Calls_<Function|Method>_Method` edge whose target qualified_name
/// contains `callee_contains`, as (caller_id, target_qn, confidence,
/// resolution_method) — BOTH caller labels are queried because palier 3
/// (unlike paliers 1-2) resolves a local receiver call from a free
/// `Function` caller exactly as well as from a `Method` one (the dy-wcet
/// `tests/`/`kani/` harnesses below are free functions).
fn calls_edges(store: &GraphStore, callee_contains: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for caller_label in ["Function", "Method"] {
        let qr = store
            .execute_query(&format!(
                "MATCH (a:{caller_label})-[r:Calls_{caller_label}_Method]->(b:Method) \
                 RETURN a.id, b.qualified_name, r.confidence, r.resolution_method",
            ))
            .expect("query Calls_*_Method");
        rows.extend(qr.rows);
    }
    rows.into_iter()
        .filter(|row| row.get(1).is_some_and(|qn| qn.contains(callee_contains)))
        .collect()
}

// ---------------------------------------------------------------------------
// Test 1 — palier 3: constructor let-binding.
// ---------------------------------------------------------------------------

#[test]
fn constructor_let_binding_receiver_resolves_via_palier_3() {
    let src = "pub struct TaskSet;\n\
               \n\
               impl TaskSet {\n\
               \x20\x20\x20\x20pub fn new() -> TaskSet { TaskSet }\n\
               \x20\x20\x20\x20pub fn response_of(&self) -> i32 { 1 }\n\
               }\n\
               \n\
               pub fn run() -> i32 {\n\
               \x20\x20\x20\x20let s = TaskSet::new();\n\
               \x20\x20\x20\x20s.response_of()\n\
               }\n";
    let (store, res, _root) = index_and_resolve("ctor", &[("src/lib.rs", src)]);

    let edges = calls_edges(&store, "response_of");
    assert_eq!(
        edges.len(),
        1,
        "expected exactly one Calls edge into response_of; unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
    let confidence: f64 = edges[0][2].parse().unwrap_or(0.0);
    assert!(
        (confidence - 0.87).abs() < 1e-9,
        "expected ReceiverLocalBinding confidence (0.87), got {confidence}"
    );
    assert_eq!(edges[0][3], "receiver-local-binding");
}

// ---------------------------------------------------------------------------
// Test 2 — palier 3: typed reference parameter.
// ---------------------------------------------------------------------------

#[test]
fn typed_reference_parameter_receiver_resolves_via_palier_3() {
    let src = "pub struct TaskSet;\n\
               \n\
               impl TaskSet {\n\
               \x20\x20\x20\x20pub fn response_of(&self) -> i32 { 1 }\n\
               }\n\
               \n\
               pub fn total(s: &TaskSet) -> i32 {\n\
               \x20\x20\x20\x20s.response_of()\n\
               }\n";
    let (store, res, _root) = index_and_resolve("param", &[("src/lib.rs", src)]);

    let edges = calls_edges(&store, "response_of");
    assert_eq!(
        edges.len(),
        1,
        "expected exactly one Calls edge into response_of; unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
    assert_eq!(edges[0][3], "receiver-local-binding");
}

// ---------------------------------------------------------------------------
// Test 3 — palier 3: homonymous types, same-file preference / Ambiguous.
// ---------------------------------------------------------------------------

const HOMONYM_A: &str = "pub struct TaskSet;\n\
    \n\
    impl TaskSet {\n\
    \x20\x20\x20\x20pub fn m(&self) -> i32 { 1 }\n\
    }\n\
    \n\
    pub fn caller() -> i32 {\n\
    \x20\x20\x20\x20let s = TaskSet::new();\n\
    \x20\x20\x20\x20s.m()\n\
    }\n\
    \n\
    impl TaskSet {\n\
    \x20\x20\x20\x20pub fn new() -> TaskSet { TaskSet }\n\
    }\n";
const HOMONYM_B: &str = "pub struct TaskSet;\n\
    \n\
    impl TaskSet {\n\
    \x20\x20\x20\x20pub fn m(&self) -> i32 { 2 }\n\
    \x20\x20\x20\x20pub fn new() -> TaskSet { TaskSet }\n\
    }\n";
const HOMONYM_C: &str = "pub struct TaskSet;\n\
    \n\
    impl TaskSet {\n\
    \x20\x20\x20\x20pub fn new() -> TaskSet { TaskSet }\n\
    }\n\
    \n\
    pub fn caller() -> i32 {\n\
    \x20\x20\x20\x20let s = TaskSet::new();\n\
    \x20\x20\x20\x20s.m()\n\
    }\n";

#[test]
fn homonymous_types_palier3_prefers_the_callers_own_file() {
    let (store, res, _root) = index_and_resolve(
        "homonyms_p3_same_file",
        &[("a.rs", HOMONYM_A), ("b.rs", HOMONYM_B)],
    );
    // `caller()` is a top-level free function (not inside `impl`), so its
    // label is `Function`, not `Method`. Filtered to `::m` targets: `caller`
    // ALSO calls `TaskSet::new()` (a plain associated-function call, not a
    // receiver call — outside palier 3's scope), which resolves via the
    // pre-existing general by-name/same-file resolution to its own file's
    // `new`; that edge is real and correct, and asserting on the full,
    // unfiltered edge set from `caller` would wrongly count it as a second
    // palier-3 resolution. This mirrors how `calls_edges` (the paliers 1-2
    // sibling suite and test 4 below) filters by callee to isolate the
    // resolution under test.
    let qr = store
        .execute_query(
            "MATCH (caller:Function)-[r:Calls_Function_Method]->(target:Method) \
             WHERE caller.qualified_name = 'a.rs::caller' \
             RETURN target.qualified_name, r.resolution_method",
        )
        .expect("query a.rs caller edges");
    let m_rows: Vec<_> = qr
        .rows
        .iter()
        .filter(|row| row[0].ends_with("::m"))
        .collect();
    assert_eq!(
        m_rows.len(),
        1,
        "a.rs::caller must resolve to exactly one m() target; unresolved={:?}, all edges={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>(),
        qr.rows
    );
    assert_eq!(m_rows[0][0], "a.rs::TaskSet::m");
    assert_eq!(m_rows[0][1], "receiver-local-binding");
}

#[test]
fn homonymous_types_palier3_caller_in_neither_file_is_ambiguous() {
    let (store, res, _root) = index_and_resolve(
        "homonyms_p3_ambiguous",
        &[
            ("a.rs", HOMONYM_A),
            ("b.rs", HOMONYM_B),
            ("c.rs", HOMONYM_C),
        ],
    );
    // Filtered to `::m` targets for the same reason as the sibling test
    // above: `c.rs::caller` also calls `TaskSet::new()`, which HOMONYM_C
    // defines locally (needed so the constructor pattern gives
    // `typed_local_bindings` a hint type at all) and which correctly
    // resolves, via the pre-existing general same-file preference, to
    // `c.rs::TaskSet::new` — that edge is real and outside palier 3's scope;
    // only the `m()` receiver call is this test's subject, and it must stay
    // unresolved (Ambiguous) since c.rs is neither a.rs nor b.rs.
    let qr = store
        .execute_query(
            "MATCH (caller:Function)-[r:Calls_Function_Method]->(target:Method) \
             WHERE caller.qualified_name = 'c.rs::caller' \
             RETURN target.qualified_name",
        )
        .expect("query c.rs caller edges");
    let m_rows: Vec<_> = qr
        .rows
        .iter()
        .filter(|row| row[0].ends_with("::m"))
        .collect();
    assert!(
        m_rows.is_empty(),
        "c.rs::caller must NOT resolve m() — two homonymous m() candidates in a.rs/b.rs, \
         c.rs is neither: {:?} (all edges={:?})",
        m_rows,
        qr.rows
    );
    let ambiguous = res.unresolved.iter().find(|u| {
        u.kind == "Calls" && u.from_id.starts_with("c.rs::caller") && u.target_text == "s.m"
    });
    assert!(
        ambiguous.is_some(),
        "expected an unresolved (ambiguous) Calls ref from c.rs::caller, got: {:?}",
        res.unresolved
            .iter()
            .map(|u| (&u.from_id, &u.target_text, &u.reason))
            .collect::<Vec<_>>()
    );
    assert!(ambiguous.unwrap().reason.starts_with("ambiguous ("));
}

// ---------------------------------------------------------------------------
// Test 4 — dy-wcet-shaped fixture: production, tests/, and kani/ callers all
// resolve statically, with zero false callers.
// ---------------------------------------------------------------------------

const DY_WCET_LIB: &str = "pub struct TaskSet;\n\
    \n\
    impl TaskSet {\n\
    \x20\x20\x20\x20pub fn new() -> TaskSet { TaskSet }\n\
    \x20\x20\x20\x20pub fn response_of(&self, i: i32) -> i32 { i }\n\
    \n\
    \x20\x20\x20\x20pub fn reorder(&self) -> i32 {\n\
    \x20\x20\x20\x20\x20\x20\x20\x20let mut trial = TaskSet::new();\n\
    \x20\x20\x20\x20\x20\x20\x20\x20trial.response_of(0)\n\
    \x20\x20\x20\x20}\n\
    }\n";
const DY_WCET_TESTS: &str = "use crate::TaskSet;\n\
    \n\
    #[test]\n\
    fn behaves() {\n\
    \x20\x20\x20\x20let s = TaskSet::new();\n\
    \x20\x20\x20\x20assert_eq!(s.response_of(0), 0);\n\
    }\n";
const DY_WCET_KANI: &str = "use crate::TaskSet;\n\
    \n\
    #[kani::proof]\n\
    fn harness() {\n\
    \x20\x20\x20\x20let mut s = TaskSet::new();\n\
    \x20\x20\x20\x20let _ = s.response_of(0);\n\
    }\n";

#[test]
fn dy_wcet_shaped_fixture_resolves_every_out_of_file_caller_with_no_false_callers() {
    let (store, res, _root) = index_and_resolve(
        "dy_wcet_shaped",
        &[
            ("src/lib.rs", DY_WCET_LIB),
            ("tests/x.rs", DY_WCET_TESTS),
            ("kani/h.rs", DY_WCET_KANI),
        ],
    );

    let edges = calls_edges(&store, "response_of");
    // The in-file production caller (`reorder`, palier 3) plus the two
    // out-of-file callers (`tests/x.rs::behaves`, `kani/h.rs::harness`) —
    // each resolving to the SAME `src/lib.rs::TaskSet::response_of` target,
    // never to any other symbol (zero false callers).
    assert_eq!(
        edges.len(),
        3,
        "expected 3 Calls edges into response_of (1 production + tests/ + kani/); \
         unresolved={:?}",
        res.unresolved.iter().map(|u| &u.reason).collect::<Vec<_>>()
    );
    for edge in &edges {
        assert_eq!(
            edge[1], "src/lib.rs::TaskSet::response_of",
            "every resolved caller must target the one real response_of — a false \
             caller would target anything else: {edges:?}"
        );
    }
}
