//! Regression (issue #327): a `fn` declared inside a function body is visible
//! only inside that function, and it shadows any same-named item or import
//! there. The resolver must bind the enclosing function's (and the nested
//! fn's own recursive) calls to it, and must never bind an unrelated call
//! elsewhere to it.
//!
//! Before the fix the nested fn had no node, so `S::m`'s `gcd(6, 4)` was
//! bound through the file's `use crate::other::gcd` import to
//! `other::gcd`: a false caller.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::collections::BTreeSet;
use std::fs;

const LIB: &str = r#"
pub mod other;
pub mod same_file;

use crate::other::gcd;

pub struct S;

impl S {
    pub fn m(&self) -> u64 {
        fn gcd(a: u64, b: u64) -> u64 {
            if b == 0 { a } else { gcd(b, a % b) }
        }
        gcd(6, 4)
    }
}

pub fn uses_imported_gcd() -> u64 {
    gcd(9, 6)
}
"#;

const OTHER: &str = r#"
pub fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}
"#;

const SAME_FILE: &str = r#"
pub fn lcm(a: u64, b: u64) -> u64 {
    a * b / lcm_gcd(a, b)
}

pub fn lcm_gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { lcm_gcd(b, a % b) }
}

pub fn shadowing() -> u64 {
    fn lcm_gcd(a: u64, _b: u64) -> u64 { a }
    lcm_gcd(1, 2)
}
"#;

/// Every function-level `Calls_*` edge in the resolved graph, as (from, to).
fn call_edges() -> BTreeSet<(String, String)> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    fs::create_dir(&src).expect("mkdir src");
    fs::write(src.join("lib.rs"), LIB).expect("write lib.rs");
    fs::write(src.join("other.rs"), OTHER).expect("write other.rs");
    fs::write(src.join("same_file.rs"), SAME_FILE).expect("write same_file.rs");
    let graph = tmp.path().join("graph");
    indexer::index_codebase(tmp.path(), &graph).expect("index");
    let store = GraphStore::open_or_create(&graph).expect("open");
    resolver::resolve_graph(&store).expect("resolve");
    let rows = store
        .execute_query(
            "MATCH (a)-[r]->(b) WHERE label(r) IN ['Calls_Function_Function', \
             'Calls_Method_Function', 'Calls_Function_Method', 'Calls_Method_Method'] \
             RETURN a.id, b.id",
        )
        .expect("query")
        .rows;
    rows.into_iter()
        .map(|row| (row[0].clone(), row[1].clone()))
        .collect()
}

fn edge(from: &str, to: &str) -> (String, String) {
    (from.to_string(), to.to_string())
}

#[test]
fn nested_fn_shadows_an_import_and_is_invisible_outside_its_function() {
    let edges = call_edges();
    let nested = "src/lib.rs::S::m::gcd";
    assert!(
        edges.contains(&edge("src/lib.rs::S::m", nested)),
        "{edges:?}"
    );
    assert!(edges.contains(&edge(nested, nested)), "{edges:?}");
    assert!(
        !edges.contains(&edge("src/lib.rs::S::m", "src/other.rs::gcd")),
        "the nested gcd shadows the imported one inside m: {edges:?}"
    );
    assert!(
        edges.contains(&edge("src/lib.rs::uses_imported_gcd", "src/other.rs::gcd")),
        "outside m, gcd is the imported one: {edges:?}"
    );
    assert!(
        !edges.contains(&edge("src/lib.rs::uses_imported_gcd", nested)),
        "a nested fn is never visible outside its function: {edges:?}"
    );
}

#[test]
fn nested_fn_does_not_make_a_same_file_top_level_call_ambiguous() {
    let edges = call_edges();
    let top = "src/same_file.rs::lcm_gcd";
    let nested = "src/same_file.rs::shadowing::lcm_gcd";
    assert!(
        edges.contains(&edge("src/same_file.rs::lcm", top)),
        "{edges:?}"
    );
    assert!(edges.contains(&edge(top, top)), "{edges:?}");
    assert!(
        edges.contains(&edge("src/same_file.rs::shadowing", nested)),
        "{edges:?}"
    );
    assert!(
        !edges.contains(&edge("src/same_file.rs::shadowing", top)),
        "{edges:?}"
    );
    assert!(
        !edges.contains(&edge("src/same_file.rs::lcm", nested)),
        "{edges:?}"
    );
}
