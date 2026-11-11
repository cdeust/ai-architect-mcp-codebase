// py_constructed_local_receiver_444 — issue #444: in Python, `w = Worker()`
// then `w.run(3)` left `w.run` unresolved as soon as two classes own `run`,
// because the local-receiver typing (`CallSite.receiver_hint`) was produced for
// Rust, C++ and TypeScript/JavaScript only. Real indexer + resolver, no LSP.
//
// Every fixture has two classes that own `run`, so an edge into one of them
// can only come from the receiver's type, never from a name-only match.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;
mod common;
use common::TempDirExt;

/// Writes `files` under a fresh tempdir's `src/`, indexes and resolves it.
fn index_and_resolve(
    tag: &str,
    files: &[(&str, &str)],
) -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    let root = tempfile::Builder::new()
        .prefix(&format!("py_ctor_receiver_{tag}_"))
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

/// (target, resolution method) of every call edge into a Method out of the
/// function or method `caller_qn`, sorted.
fn method_calls_from(store: &GraphStore, caller_qn: &str) -> Vec<(String, String)> {
    let mut edges = Vec::new();
    for label in ["Function", "Method"] {
        let rows = store
            .execute_query(&format!(
                "MATCH (a:{label})-[r:Calls_{label}_Method]->(b:Method) \
                 WHERE a.qualified_name = '{caller_qn}' \
                 RETURN b.qualified_name, r.resolution_method"
            ))
            .expect("query call edges")
            .rows;
        edges.extend(rows.into_iter().map(|r| (r[0].clone(), r[1].clone())));
    }
    edges.sort();
    edges
}

/// The per-site state of the calls spelled `callee` made by `caller_qn`:
/// (is_resolved, target of the per-site row or empty), sorted.
fn sites_of(store: &GraphStore, caller_qn: &str, callee: &str) -> Vec<(String, String)> {
    let site = format!(
        "(c:CallSite) WHERE starts_with(c.id, '{caller_qn}::call@') \
         AND c.callee_name = '{callee}'"
    );
    let column = |cypher: String| -> Vec<Vec<String>> {
        store.execute_query(&cypher).expect("query call sites").rows
    };
    let targets = column(format!(
        "MATCH (m:Method)<-[:Calls_CallSite_Method]-{site} RETURN c.id, m.qualified_name"
    ));
    let mut sites: Vec<(String, String)> =
        column(format!("MATCH {site} RETURN c.id, c.is_resolved"))
            .into_iter()
            .map(|flag| {
                let target = targets.iter().find(|t| t[0] == flag[0]);
                (
                    flag[1].to_lowercase(),
                    target.map_or(String::new(), |t| t[1].clone()),
                )
            })
            .collect();
    sites.sort();
    sites
}

fn bound(target: &str) -> Vec<(String, String)> {
    vec![(target.to_string(), "receiver-local-binding".to_string())]
}

const WORKERS: &str = "\
class Worker:
    def run(self, n):
        return n


class Other:
    def run(self, n):
        return -n


def use_worker():
    w = Worker()
    return w.run(3)


def use_other():
    o = Other()
    return o.run(3)


class Pool:
    def drain(self):
        w = Worker()
        return w.run(1)


def rebound():
    w = Worker()
    w = Other()
    return w.run(3)


def from_a_call():
    w = make()
    return w.run(3)


def parameter(w):
    return w.run(3)
";

#[test]
fn a_local_built_from_a_class_of_the_file_binds_to_that_class() {
    let (store, _res, _root) = index_and_resolve("bind", &[("workers.py", WORKERS)]);
    assert_eq!(
        method_calls_from(&store, "workers.py::use_worker"),
        bound("workers.py::Worker::run")
    );
    assert_eq!(
        method_calls_from(&store, "workers.py::use_other"),
        bound("workers.py::Other::run")
    );
    // A method is a caller like a function.
    assert_eq!(
        method_calls_from(&store, "workers.py::Pool::drain"),
        bound("workers.py::Worker::run")
    );
    assert_eq!(
        sites_of(&store, "workers.py::use_worker", "w.run"),
        [("true".to_string(), "workers.py::Worker::run".to_string())]
    );
}

#[test]
fn a_receiver_the_function_does_not_prove_stays_unresolved() {
    let (store, _res, _root) = index_and_resolve("neg", &[("workers.py", WORKERS)]);
    // Rebound, initialised by a call that is not a class of the file, or a
    // parameter: with two classes owning `run`, no edge and an open site.
    for caller in ["rebound", "from_a_call", "parameter"] {
        let caller_qn = format!("workers.py::{caller}");
        assert_eq!(
            method_calls_from(&store, &caller_qn),
            Vec::<(String, String)>::new(),
            "{caller} must not bind"
        );
        assert_eq!(
            sites_of(&store, &caller_qn, "w.run"),
            [("false".to_string(), String::new())],
            "{caller} stays open"
        );
    }
}

#[test]
fn an_ambiguous_class_stays_unresolved() {
    // Two classes named `Worker` in the file (the second rebinds the name),
    // each with `run`: the constructor does not name one class, so no hint,
    // and the by-name path sees three `run` and declines.
    let twice = "\
class Worker:
    def run(self, n):
        return n


class Worker:
    def run(self, n):
        return n + 1


class Other:
    def run(self, n):
        return -n


def use_worker():
    w = Worker()
    return w.run(3)
";
    let (store, _res, _root) = index_and_resolve("twice", &[("workers.py", twice)]);
    assert_eq!(
        method_calls_from(&store, "workers.py::use_worker"),
        Vec::<(String, String)>::new()
    );
}

#[test]
fn a_class_of_another_file_is_not_bound_by_this_gate() {
    // `Worker` is imported: the file does not declare it, so the parser emits
    // no hint and the call keeps the answer it had (open: two `run` exist).
    let (store, _res, _root) = index_and_resolve(
        "imported",
        &[
            ("workers.py", WORKERS),
            (
                "client.py",
                "from workers import Worker\n\n\ndef go():\n    w = Worker()\n    return w.run(3)\n",
            ),
        ],
    );
    assert_eq!(
        method_calls_from(&store, "client.py::go"),
        Vec::<(String, String)>::new()
    );
}

#[test]
fn a_method_the_class_inherits_is_not_bound_by_this_gate() {
    // `Child` does not define `run`: the hint would name a class without that
    // method, so none is emitted and the call keeps the answer it had before
    // this gate (open: inherited methods are a later slice of issue #444).
    let inherited = "\
class Base:
    def run(self, n):
        return n


class Child(Base):
    pass


def go():
    c = Child()
    return c.run(3)
";
    let (store, _res, _root) = index_and_resolve("inherited", &[("shapes.py", inherited)]);
    assert_eq!(
        method_calls_from(&store, "shapes.py::go"),
        Vec::<(String, String)>::new()
    );
}
