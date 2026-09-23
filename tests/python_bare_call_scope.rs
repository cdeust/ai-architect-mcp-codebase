// python_bare_call_scope — issue #335 follow-up. A bare Python call `f()`
// inside a method is looked up in the local, enclosing-function, module and
// builtin scopes, never in the class body (Python Language Reference §4.2.2).
// The resolver treated a same-named method as a rival candidate, so the real
// call to the module function was dropped as ambiguous (pg_store.py
// `_now_iso()`), and a bare call whose only same-named symbol is a method
// was bound to that method although Python raises NameError there.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
mod common;
use common::TempDirExt;

const FIXTURE: &str = "def now():
    return 1


class Clock:
    def now(self):
        return 2

    def tick(self):
        return now()


class Orphan:
    def helper(self):
        return 3

    def go(self):
        return helper()
";

fn index_and_resolve() -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("python_bare_call_scope_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture");
    fs::create_dir_all(&src).expect("mkdir fixture");
    fs::write(src.join("clock.py"), FIXTURE).expect("write fixture");
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    (store, tmp)
}

/// Every Calls edge out of `caller`, as (table, target id).
fn calls_from(store: &GraphStore, caller: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for table in ["Calls_Method_Function", "Calls_Method_Method"] {
        let q = format!("MATCH (a)-[:{table}]->(b) WHERE a.id = '{caller}' RETURN b.id");
        for row in store.execute_query(&q).expect("query calls").rows {
            out.push((table.to_string(), row[0].clone()));
        }
    }
    out
}

#[test]
fn a_bare_call_shadowed_by_a_method_name_was_dropped_as_ambiguous() {
    let (store, _tmp) = index_and_resolve();
    assert_eq!(
        calls_from(&store, "clock.py::Clock::tick"),
        vec![(
            "Calls_Method_Function".to_string(),
            "clock.py::now".to_string()
        )],
        "`now()` in a method names the module function, never `Clock.now`"
    );
    let per_site = store
        .execute_query(
            "MATCH (cs:CallSite)-[:Calls_CallSite_Function]->(f) \
             WHERE cs.line = 10 RETURN f.id",
        )
        .expect("query per-site row");
    assert_eq!(per_site.rows, vec![vec!["clock.py::now".to_string()]]);
}

#[test]
fn a_bare_call_was_bound_to_a_method_python_cannot_reach() {
    let (store, _tmp) = index_and_resolve();
    assert_eq!(
        calls_from(&store, "clock.py::Orphan::go"),
        Vec::<(String, String)>::new(),
        "`helper()` is a NameError in Python; it must not bind to `Orphan.helper`"
    );
}
