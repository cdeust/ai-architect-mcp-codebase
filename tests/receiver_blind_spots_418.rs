// receiver_blind_spots_418 — issue #418 over the real indexer + resolver: four limits
// of the Rust receiver typing found while reviewing #417, each reproduced before any
// change. A call the file does not prove must stay unresolved (a wrong single edge is
// worse than none).
//
// Every fixture has two owners of `ok`, so an edge into one of them can only come from
// the receiver's type, never from a name-only match.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;
mod common;
use common::TempDirExt;

const LIB: &str = "pub struct T;
impl T {
    pub fn ok(&self) -> bool {
        true
    }
}
pub struct Real;
impl Real {
    pub fn ok(&self) -> bool {
        true
    }
}
pub trait Tr {
    fn ok(&self) -> bool;
}
// 1: `T` is a generic parameter here, not the struct `T` of the file.
pub fn generic_shadow<T: Tr>(x: T) -> bool {
    x.ok() // generic-shadow
}

#[derive(Clone)]
pub struct Cl;
impl Cl {
    pub fn ok(&self) -> bool {
        true
    }
}
pub struct Other;
impl Other {
    pub fn ok(&self) -> bool {
        true
    }
}
macro_rules! other_clone {
    ($t:ty) => {
        impl $t {
            pub fn clone(&self) -> Other {
                Other
            }
        }
    };
}
other_clone!(Cl);
// 2: a macro gave `Cl` an inherent `clone` that returns `Other`.
pub fn macro_clone(c: Cl) -> bool {
    let d = c.clone();
    d.ok() // macro-clone
}

// 3: the words `derive` and `Clone` sit in a doc attribute, not in a derive list.
#[doc = \"never derive Clone here\"]
pub struct Doc;
impl Doc {
    pub fn ok(&self) -> bool {
        true
    }
}
pub fn doc_clone(d: Doc) -> bool {
    let e = d.clone();
    e.ok() // doc-clone
}

// 4: `my::Option` is not `Option`; its `Some` binds something else.
pub fn build() -> my::Option<Real> {
    my::make()
}
pub fn path_option() -> bool {
    if let Some(s) = build() {
        return s.ok(); // path-option
    }
    false
}
";

fn index_and_resolve(src: &str) -> (GraphStore, common::TestTempDir) {
    let root = tempfile::Builder::new()
        .prefix("receiver_blind_spots_418_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let p = root.join("src/lib.rs");
    fs::create_dir_all(p.parent().expect("parent")).expect("create fixture dir");
    fs::write(&p, src).expect("write fixture file");
    let graph_dir = root.join("graph");
    indexer::index_codebase(&root, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve_graph");
    (store, root)
}

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

fn targets(edges: &[(String, String)], caller: &str) -> Vec<String> {
    edges
        .iter()
        .filter(|(a, _)| a == caller)
        .map(|(_, b)| b.clone())
        .collect()
}

#[test]
fn a_generic_parameter_is_not_the_struct_that_shares_its_name() {
    let (store, _root) = index_and_resolve(LIB);
    let edges = ok_edges(&store);
    assert!(
        targets(&edges, "generic_shadow").is_empty(),
        "x: T with T generic must stay open; edges: {:?}",
        targets(&edges, "generic_shadow")
    );
}

#[test]
fn a_clone_a_macro_may_define_declines() {
    let (store, _root) = index_and_resolve(LIB);
    let edges = ok_edges(&store);
    assert!(
        targets(&edges, "macro_clone").is_empty(),
        "a macro-generated clone is unproven; edges: {:?}",
        targets(&edges, "macro_clone")
    );
}

#[test]
fn clone_with_a_derive_list_resolves() {
    let src = "#[derive(Debug, Clone)]
pub struct A;
impl A { pub fn ok(&self) -> bool { true } }
pub struct B;
impl B { pub fn ok(&self) -> bool { true } }
pub fn go(a: A) -> bool {
    let c = a.clone();
    c.ok()
}
";
    let (store, _root) = index_and_resolve(src);
    let edges = ok_edges(&store);
    assert_eq!(targets(&edges, "go"), vec!["src/lib.rs::A::ok".to_string()]);
}

#[test]
fn a_path_qualified_option_is_not_the_std_option() {
    let (store, _root) = index_and_resolve(LIB);
    let edges = ok_edges(&store);
    assert!(
        targets(&edges, "path_option").is_empty(),
        "my::Option<Real> must not unwrap like Option; edges: {:?}",
        targets(&edges, "path_option")
    );
}
