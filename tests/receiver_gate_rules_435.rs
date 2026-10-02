// receiver_gate_rules_435 — the path rules of `resolver::calls::gates` over the real
// indexer + resolver (the survivors the review of #435 listed). The label and the
// confidence of a row say which rule answered: `receiver-return-type` (0.85) is the
// weaker evidence of a hint read off a return type, `receiver-local-binding` (0.87)
// the evidence of a hint read off the binding itself.
//
// Some rules guard shapes the Rust parser never emits (a path hint whose origin is a
// `use crate::` import, a constructor read through `use super::*` next to a namesake
// in the same file). The resolver reads whatever `CallSite` rows the graph holds, so
// those tests rewrite one row's hint before resolving: the contract under test is the
// resolver's, not the parser's.
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
mod common;
use common::TempDirExt;

const LIB: &str = "mod other;
pub struct Set;
impl Set {
    pub fn new() -> Self {
        Set
    }
    pub fn m(&self) -> u8 {
        1
    }
}
pub struct Tier(pub u8);
impl Tier {
    pub fn m(&self) -> u8 {
        1
    }
}
pub mod z {
    pub struct Tier(pub u8);
    impl Tier {
        pub fn m(&self) -> u8 {
            3
        }
    }
}
pub mod shapes {
    pub struct Big;
    impl Big {
        pub fn new() -> Self {
            Big
        }
        pub fn m(&self) -> u8 {
            1
        }
    }
}
pub mod a {
    use super::*;
    pub fn in_place() -> u8 {
        Set::new().m() // a-inplace
    }
    pub fn tuple() -> u8 {
        Tier(1).m() // a-tuple
    }
}
pub mod user {
    use crate::shapes::Big;
    pub fn make() -> Big {
        Big::new()
    }
    pub fn call() -> u8 {
        let s = make();
        s.m() // local-import
    }
}
pub mod paths {
    pub fn make_path() -> crate::shapes::Big {
        crate::shapes::Big::new()
    }
    pub fn call() -> u8 {
        let s = make_path();
        s.m() // path-return
    }
}
";

const OTHER: &str = "pub struct Set;
impl Set {
    pub fn m(&self) -> u8 {
        2
    }
}
pub struct Tier(pub u8);
impl Tier {
    pub fn m(&self) -> u8 {
        2
    }
}
pub struct Big;
impl Big {
    pub fn m(&self) -> u8 {
        2
    }
}
";

/// A rewritten hint: the call on the line marked `marker` gets `hint` and `via`.
type Rewrite = (&'static str, &'static str, &'static str);

fn index_and_resolve(rewrites: &[Rewrite]) -> (GraphStore, common::TestTempDir) {
    let root = tempfile::Builder::new()
        .prefix("receiver_gate_rules_435_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"gr435\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(root.join("src/lib.rs"), LIB).expect("write lib.rs");
    fs::write(root.join("src/other.rs"), OTHER).expect("write other.rs");
    let graph_dir = root.join("graph");
    indexer::index_codebase(&root, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    for (marker, hint, via) in rewrites {
        store
            .execute_query(&format!(
                "MATCH (cs:CallSite) WHERE cs.line = {} AND cs.callee_name ENDS WITH '.m' \
                 SET cs.receiver_hint = '{hint}', cs.receiver_hint_via = '{via}'",
                line_of(marker)
            ))
            .expect("rewrite the hint");
    }
    resolver::resolve_graph(&store).expect("resolve_graph");
    (store, root)
}

fn line_of(marker: &str) -> usize {
    LIB.lines()
        .position(|l| l.contains(&format!("// {marker}")))
        .unwrap_or_else(|| panic!("no line holds // {marker}"))
        + 1
}

/// `(target id, resolution_method, confidence)` of the rows of the `.m()` call on
/// the line marked `marker`.
fn rows_of(store: &GraphStore, marker: &str) -> Vec<(String, String, String)> {
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) WHERE cs.line = {} \
             AND cs.callee_name ENDS WITH '.m' \
             RETURN t.id, r.resolution_method, r.confidence",
            line_of(marker)
        ))
        .unwrap_or_else(|e| panic!("query {marker}: {e}"))
        .rows
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone(), r[2].clone()))
        .collect()
}

fn row(target: &str, method: &str, confidence: &str) -> Vec<(String, String, String)> {
    vec![(
        target.to_string(),
        method.to_string(),
        confidence.to_string(),
    )]
}

const BIG: &str = "src/lib.rs::shapes::Big::m";
const RETURN_TYPE: (&str, &str) = ("receiver-return-type", "0.85");
const BINDING: (&str, &str) = ("receiver-local-binding", "0.87");

fn expect(target: &str, (method, confidence): (&str, &str)) -> Vec<(String, String, String)> {
    row(target, method, confidence)
}

#[test]
fn a_return_type_shown_by_a_use_crate_path_gets_the_return_type_evidence() {
    let (store, _root) = index_and_resolve(&[]);
    assert_eq!(rows_of(&store, "local-import"), expect(BIG, RETURN_TYPE));
}

#[test]
fn a_constructor_read_through_a_path_keeps_the_evidence_of_what_it_was_read_off() {
    let (store, _root) = index_and_resolve(&[]);
    // `Set::new().m()`: the type comes from the declared return type of `new`.
    assert_eq!(
        rows_of(&store, "a-inplace"),
        expect("src/lib.rs::Set::m", RETURN_TYPE)
    );
}

#[test]
fn a_hint_written_as_a_path_whose_origin_is_a_return_type_is_not_read_as_a_written_path() {
    let (store, _root) = index_and_resolve(&[("path-return", "crate::shapes::Big", "return-type")]);
    assert_eq!(rows_of(&store, "path-return"), expect(BIG, RETURN_TYPE));
}

#[test]
fn a_hint_written_as_a_path_whose_origin_is_a_use_crate_import_keeps_the_lookup_by_name() {
    // The `use` says `crate::other::Big`; the hint, a path of its own, is the type
    // the lookup by name finds in this file. The import path must not redirect it.
    let (store, _root) = index_and_resolve(&[(
        "path-return",
        "crate::shapes::Big",
        "return-type-local-import:crate::other::Big",
    )]);
    assert_eq!(rows_of(&store, "path-return"), expect(BIG, RETURN_TYPE));
}

#[test]
fn a_constructor_hint_is_read_by_the_path_of_the_name_not_by_every_namesake_of_the_file() {
    // `Tier` is defined twice in the file (root and `z`): the caller's `use super::*`
    // names the root one, so the call resolves there. The lookup by file alone would
    // find both and decline.
    let (store, _root) = index_and_resolve(&[("a-tuple", "Tier", "constructed")]);
    assert_eq!(
        rows_of(&store, "a-tuple"),
        expect("src/lib.rs::Tier::m", BINDING)
    );
}
