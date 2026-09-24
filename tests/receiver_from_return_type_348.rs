// receiver_from_return_type_348 — issues #348 and #349: a local receiver bound
// from a free function whose return type is declared (`let s = make();`,
// `let Some(s) = build() else { .. }`, `build().expect(..)`) was left
// unresolved by the static resolver although the language-server pass resolves
// it. These tests pin the new resolution, its own evidence tier (below the
// local-binding tier), the shapes that must stay unresolved (a wrong single
// edge is worse than none), and that a second resolve adds nothing.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{clustering, indexer, resolver};
use std::fs;
mod common;
use common::TempDirExt;

/// Each `answer(N)` marker sits on its own line; `line_of` finds it.
const FIXTURE: &str = "pub struct Set {
    n: u32,
}

impl Set {
    pub fn new() -> Set {
        Set { n: 0 }
    }

    pub fn answer(&self, i: u32) -> u32 {
        self.n + i
    }
}

pub fn make() -> Set {
    Set::new()
}

pub fn build(ok: bool) -> Option<Set> {
    if ok {
        Some(Set::new())
    } else {
        None
    }
}

pub fn generic<T: Default>() -> T {
    T::default()
}

pub fn control_constructor() -> u32 {
    let s = Set::new();
    s.answer(1)
}

pub fn via_free_function() -> u32 {
    let s = make();
    s.answer(2)
}

pub fn via_let_else() -> u32 {
    let Some(s) = build(true) else { return 0 };
    s.answer(3)
}

pub fn via_expect() -> u32 {
    let s = build(true).expect(\"admitted\");
    s.answer(4)
}

pub fn option_not_unwrapped() -> u32 {
    let s = build(true);
    s.answer(5)
}

pub fn generic_return() -> u32 {
    let s = generic();
    s.answer(6)
}

pub fn shadowed_receiver() -> u32 {
    let s = make();
    let s = build(true).expect(\"admitted\");
    s.answer(7)
}

pub fn unwrapping_a_plain_type() -> u32 {
    let s = make().unwrap();
    s.answer(8)
}

mod a {
    pub struct A;
    impl A {
        pub fn answer(&self) -> u32 {
            1
        }
    }
    pub fn dup() -> A {
        A
    }
}

mod b {
    pub struct B;
    impl B {
        pub fn answer(&self) -> u32 {
            2
        }
    }
    pub fn dup() -> B {
        B
    }
}

pub fn two_functions_of_one_name() -> u32 {
    let s = dup();
    s.answer(9)
}
";

const TARGET: &str = "src/lib.rs::Set::answer";

fn line_of(marker: &str) -> String {
    let index = FIXTURE
        .lines()
        .position(|l| l.contains(marker))
        .unwrap_or_else(|| panic!("no line holds {marker}"));
    (index + 1).to_string()
}

fn index_and_resolve() -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("receiver_from_return_type_348_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture/src");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::write(src.join("lib.rs"), FIXTURE).expect("write fixture");
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&tmp.path().join("fixture"), &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    (store, tmp)
}

/// (is_resolved, receiver_hint, receiver_hint_via) of the `answer` call site
/// on `line`.
fn site(store: &GraphStore, line: &str) -> (String, String, String) {
    let qr = store
        .execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.line = {line} AND cs.callee_name ENDS WITH '.answer' \
             RETURN cs.is_resolved, cs.receiver_hint, cs.receiver_hint_via"
        ))
        .unwrap_or_else(|e| panic!("query site {line}: {e}"));
    assert_eq!(qr.rows.len(), 1, "one `answer` call site on line {line}");
    let r = &qr.rows[0];
    (r[0].clone(), r[1].clone(), r[2].clone())
}

/// (target id, resolution_method, confidence) of the per-site rows of `line`.
fn rows(store: &GraphStore, line: &str) -> Vec<(String, String, String)> {
    let qr = store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) WHERE cs.line = {line} \
             RETURN t.id, r.resolution_method, r.confidence"
        ))
        .unwrap_or_else(|e| panic!("query rows {line}: {e}"));
    qr.rows
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone(), r[2].clone()))
        .collect()
}

fn all_rows(store: &GraphStore) -> Vec<Vec<String>> {
    let mut rows = store
        .execute_query(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) \
             RETURN cs.line, t.id, r.resolution_method, r.confidence",
        )
        .expect("query all per-site rows")
        .rows;
    rows.sort();
    rows
}

/// The constructor form keeps its tier: nothing about it may change.
#[test]
fn a_receiver_typed_by_a_constructor_keeps_the_local_binding_tier() {
    let (store, _tmp) = index_and_resolve();
    let line = line_of("answer(1)");
    assert_eq!(
        site(&store, &line),
        ("true".into(), "Set".into(), "".into())
    );
    assert_eq!(
        rows(&store, &line),
        vec![(
            TARGET.into(),
            "receiver-local-binding".into(),
            "0.87".into()
        )]
    );
}

/// Issue #348: the free function's declared return type types the receiver.
#[test]
fn a_receiver_from_a_free_function_was_left_unresolved() {
    let (store, _tmp) = index_and_resolve();
    let line = line_of("answer(2)");
    assert_eq!(
        site(&store, &line),
        ("true".into(), "Set".into(), "return-type".into())
    );
    assert_eq!(
        rows(&store, &line),
        vec![(TARGET.into(), "receiver-return-type".into(), "0.85".into())]
    );
}

/// Issue #349: an `Option` is unwrapped only through a form the source spells.
#[test]
fn a_receiver_from_let_else_and_expect_was_left_unresolved() {
    let (store, _tmp) = index_and_resolve();
    for marker in ["answer(3)", "answer(4)"] {
        let line = line_of(marker);
        assert_eq!(
            site(&store, &line),
            ("true".into(), "Set".into(), "return-type".into()),
            "{marker}"
        );
        assert_eq!(
            rows(&store, &line),
            vec![(TARGET.into(), "receiver-return-type".into(), "0.85".into())],
            "{marker}"
        );
    }
}

/// A wrong single edge is worse than none: every one of these shapes must
/// stay without an edge, a hint and a per-site row.
#[test]
fn a_receiver_whose_type_the_signature_does_not_give_stays_unresolved() {
    let (store, _tmp) = index_and_resolve();
    for (marker, why) in [
        (
            "answer(5)",
            "an Option used without unwrapping is not the wrapped type",
        ),
        ("answer(6)", "a generic return type names no concrete type"),
        ("answer(7)", "a name bound twice has no single binding"),
        ("answer(8)", "unwrapping a plain type is an unknown method"),
        (
            "answer(9)",
            "two functions of the name leave the callee in doubt",
        ),
    ] {
        let line = line_of(marker);
        assert_eq!(
            site(&store, &line),
            ("false".into(), "".into(), "".into()),
            "{why}"
        );
        assert!(rows(&store, &line).is_empty(), "{why}");
    }
}

/// `get_impact` lists the callers found through the return type, and only
/// with the confidence of their own tier.
#[test]
fn get_impact_lists_the_callers_found_through_the_return_type() {
    let (store, _tmp) = index_and_resolve();
    let impact = clustering::get_impact(&store, TARGET).expect("impact");
    let mut callers: Vec<(String, String)> = impact
        .callers
        .iter()
        .map(|n| (n.qualified_name.clone(), format!("{:.2}", n.confidence)))
        .collect();
    callers.sort();
    assert_eq!(
        callers,
        vec![
            ("src/lib.rs::control_constructor".into(), "0.87".into()),
            ("src/lib.rs::via_expect".into(), "0.85".into()),
            ("src/lib.rs::via_free_function".into(), "0.85".into()),
            ("src/lib.rs::via_let_else".into(), "0.85".into()),
        ]
    );
}

/// A second resolve writes nothing new.
#[test]
fn a_second_resolve_adds_no_per_site_row_for_the_return_type_tier() {
    let (store, _tmp) = index_and_resolve();
    let first = all_rows(&store);
    resolver::resolve_graph(&store).expect("second resolve");
    assert_eq!(all_rows(&store), first);
}
