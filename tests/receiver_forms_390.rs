// receiver_forms_390 — issue #390 over the real indexer + resolver: a Rust receiver
// typed by `x.clone()`, by `if let Some(x) = f(..)`, by the untyped parameter of a
// closure given to an `Option` method, or by the result of a local closure resolves
// to the one method of the type; a look-alike the file does not prove stays
// unresolved (a wrong single edge is worse than none).
//
// Two types of the file own an `ok` method, so a name-only match would be
// ambiguous: an edge into `Set::ok` can only come from the receiver's type.
//
// source: measured on DYResearch/dy-wcet v4.1.6 (commit 8bb83ad): seven calls of
// `TaskSet::is_schedulable` rust-analyzer resolves and 0.14.0 left open.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;
mod common;
use common::TempDirExt;

const LIB: &str = "#[derive(Clone)]
pub struct Set {
    pub n: u8,
}
pub struct Other {
    pub n: u8,
}
pub struct NotClone {
    pub n: u8,
}
impl Set {
    pub fn new() -> Set {
        Set { n: 1 }
    }
    pub fn ok(&self) -> bool {
        self.n > 0
    }
    pub fn clone_of_self(&self) -> bool {
        let t = self.clone();
        t.ok() // clone-self
    }
}
impl Other {
    pub fn ok(&self) -> bool {
        self.n > 0
    }
}
pub struct Maybe;
impl Maybe {
    pub fn new() -> Option<Maybe> {
        None
    }
    pub fn ok(&self) -> bool {
        true
    }
}
impl NotClone {
    pub fn ok(&self) -> bool {
        self.n > 0
    }
    pub fn no_clone_evidence(&self) -> bool {
        let t = self.clone();
        t.ok() // decline-clone
    }
}

pub fn build() -> Option<Set> {
    Some(Set::new())
}

pub fn clone_of_binding() -> bool {
    let s = Set::new();
    let c = s.clone();
    c.ok() // clone-binding
}

pub fn if_let_some() -> bool {
    if let Some(s) = build() {
        return s.ok(); // if-let
    }
    false
}

pub fn if_let_unknown() -> bool {
    if let Some(s) = mystery() {
        return s.ok(); // decline-if-let
    }
    false
}

pub fn closure_parameter() -> bool {
    build().is_some_and(|s| s.ok()) // closure-param
}

pub fn closure_parameter_filter() -> bool {
    build().filter(|s| s.ok()).is_some() // decline-closure-param
}

pub fn closure_result() -> bool {
    let make = |n: u8| {
        let mut t = Set::new();
        t.n = n;
        t
    };
    make(2).ok() // closure-result
}

pub fn closure_result_in_macro() {
    let make = |n: u8| {
        let mut t = Set::new();
        t.n = n;
        t
    };
    assert!(make(2).ok());
}

pub fn closure_result_not_built() -> bool {
    let make = || {
        let t = Maybe::new();
        t
    };
    make().ok() // decline-closure-assoc
}

pub fn closure_result_unknown() -> bool {
    let make = |n: u8| mystery(n);
    make(2).ok() // decline-closure-result
}
";

fn index_and_resolve(files: &[(&str, &str)]) -> (GraphStore, common::TestTempDir) {
    let root = tempfile::Builder::new()
        .prefix("receiver_forms_390_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    for (rel, body) in files {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("create fixture dir");
        fs::write(&p, body).expect("write fixture file");
    }
    let graph_dir = root.join("graph");
    indexer::index_codebase(&root, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve_graph");
    (store, root)
}

/// `(caller name, callee qualified name)` of every resolved call into an `ok`.
fn ok_edges(store: &GraphStore) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for label in ["Function", "Method"] {
        let qr = store
            .execute_query(&format!(
                "MATCH (a:{label})-[r:Calls_{label}_Method]->(b:Method) \
                 RETURN a.name, b.qualified_name"
            ))
            .expect("query Calls_*_Method");
        rows.extend(qr.rows);
    }
    rows.into_iter()
        .filter(|r| r[1].ends_with("::ok"))
        .map(|r| (r[0].clone(), r[1].clone()))
        .collect()
}

fn resolved_to_set(edges: &[(String, String)], caller: &str) -> bool {
    edges
        .iter()
        .any(|(a, b)| a == caller && b.contains("Set::ok"))
}

fn edges_of(caller: &str, edges: &[(String, String)]) -> Vec<String> {
    edges
        .iter()
        .filter(|(a, _)| a == caller)
        .map(|(_, b)| b.clone())
        .collect()
}

#[test]
fn the_four_forms_resolve_to_the_type_they_prove() {
    let (store, _root) = index_and_resolve(&[("src/lib.rs", LIB)]);
    let edges = ok_edges(&store);
    for caller in [
        "clone_of_self",
        "clone_of_binding",
        "if_let_some",
        "closure_parameter",
        "closure_result",
        "closure_result_in_macro",
    ] {
        assert!(
            resolved_to_set(&edges, caller),
            "{caller} must resolve to Set::ok; edges: {edges:?}"
        );
        assert_eq!(
            edges_of(caller, &edges).len(),
            1,
            "{caller}: exactly one edge"
        );
    }
}

#[test]
fn the_look_alikes_the_file_does_not_prove_stay_unresolved() {
    let (store, _root) = index_and_resolve(&[("src/lib.rs", LIB)]);
    let edges = ok_edges(&store);
    for caller in [
        "no_clone_evidence",
        "if_let_unknown",
        "closure_parameter_filter",
        "closure_result_unknown",
        "closure_result_not_built",
    ] {
        assert!(
            edges_of(caller, &edges).is_empty(),
            "{caller} must stay unresolved; edges: {edges:?}"
        );
    }
}
