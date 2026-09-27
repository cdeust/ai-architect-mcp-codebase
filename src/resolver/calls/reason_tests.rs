//! Issue #393: every call site the resolvers leave open carries one reason from
//! the closed set, a resolved one carries none, and the counts add up. The
//! fixture is a real Cargo crate, indexed and resolved end to end, so the Cargo
//! facts (crate names, files outside every target, `#[path]` reach) are the
//! ones `cargo metadata` gives.

use crate::graph_store::callsite_reasons::{self as reasons, BY_CONSTRUCTION, IMPROVABLE};
use crate::graph_store::GraphStore;
use std::collections::BTreeMap;
use std::path::Path;

const LIB: &str = r#"use std::io;
use rand::Rng;
pub mod a;
pub mod b;
#[cfg(kani)]
#[path = "../kani/proofs.rs"]
mod proofs;

macro_rules! my_macro {
    () => {};
}

pub enum Kind {
    A(u8),
}

pub enum Answer {
    Yes(u8),
}

const LIMIT: u8 = 3;

pub type K = Kind;

pub struct Set {
    v: Vec<u64>,
}

impl Set {
    pub fn new() -> Self {
        Set { v: Vec::new() }
    }
    pub fn go(&self) {
        self.missing();
    }
}

pub fn helper() {}

pub fn writer() {
    let _w = io::BufWriter::new(io::stdout());
}

pub fn draw() -> u32 {
    rand::random()
}

pub fn text() {
    let s = String::new();
    s.push_str("a");
}

pub fn calls(x: &[u8]) -> usize {
    helper();
    dup();
    nothing_here();
    my_macro!();
    let _k = Kind::A(1);
    let _j = K::A(2);
    let _a = Answer::Yes(1);
    let _l = u8::min(LIMIT, 1);
    let _m = matches!(x.first(), Some(_));
    x.iter().count()
}
"#;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

fn cargo_available() -> bool {
    std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok()
}

/// Indexes and resolves the fixture crate; the store and the temp dir.
fn resolved_fixture() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::tempdir().expect("temp dir");
    let src = dir.path().join("crate");
    write(
        &src,
        "Cargo.toml",
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write(&src, "src/lib.rs", LIB);
    write(&src, "src/a.rs", "pub fn dup() {}\npub struct A(pub u8);\n");
    write(&src, "src/b.rs", "pub fn dup() {}\n");
    write(
        &src,
        "kani/proofs.rs",
        "fn proof() {\n    undefined_fn();\n}\n",
    );
    write(&src, "kani/orphan.rs", "fn lone() {\n    other();\n}\n");
    std::fs::create_dir_all(dir.path().join("out")).expect("mkdir out");
    let graph = dir.path().join("out/graph");
    crate::indexer::index_codebase(&src, &graph).expect("index");
    let store = GraphStore::open_or_create(&graph).expect("open");
    crate::resolver::resolve_graph(&store).expect("resolve");
    (dir, store)
}

/// `callee_name` -> (resolved, reason, detail) of every site.
fn sites(store: &GraphStore) -> BTreeMap<String, (bool, String, String)> {
    let rows = store
        .execute_query(
            "MATCH (cs:CallSite) RETURN cs.callee_name, cs.is_resolved, \
             cs.unresolved_reason, cs.unresolved_detail",
        )
        .expect("sites");
    rows.rows
        .into_iter()
        .map(|r| {
            (
                r[0].clone(),
                (r[1] == "True" || r[1] == "true", r[2].clone(), r[3].clone()),
            )
        })
        .collect()
}

fn reason_of<'a>(
    all: &'a BTreeMap<String, (bool, String, String)>,
    callee: &str,
) -> (&'a str, &'a str) {
    let (resolved, reason, detail) = all
        .get(callee)
        .unwrap_or_else(|| panic!("no site `{callee}` in {all:?}"));
    assert!(!resolved, "`{callee}` resolved: {all:?}");
    (reason.as_str(), detail.as_str())
}

/// `(callee, reason, detail)` the fixture must record, each with why.
const EXPECTED: &[(&str, &str, &str)] = &[
    ("Vec::new", reasons::REASON_EXTERNAL_CALLEE, "std"),
    ("io::BufWriter::new", reasons::REASON_EXTERNAL_CALLEE, "std"),
    ("io::stdout", reasons::REASON_EXTERNAL_CALLEE, "std"),
    ("rand::random", reasons::REASON_EXTERNAL_CALLEE, "rand"),
    ("String::new", reasons::REASON_EXTERNAL_CALLEE, "std"),
    ("s.push_str", reasons::REASON_EXTERNAL_CALLEE, "std"),
    ("u8::min", reasons::REASON_EXTERNAL_CALLEE, "std"),
    ("self.missing", reasons::REASON_NOT_FOUND, ""),
    ("dup", reasons::REASON_AMBIGUOUS, "2"),
    ("nothing_here", reasons::REASON_UNKNOWN_CALLEE, ""),
    ("matches!", reasons::REASON_NOT_A_CALL, ""),
    ("my_macro!", reasons::REASON_MACRO_SITE, "no_table_entry"),
    ("Answer::Yes", reasons::REASON_NOT_A_CALL, "enum_variant"),
    ("Kind::A", reasons::REASON_NOT_A_CALL, "enum_variant"),
    // A constant passed as an argument is referenced, not called.
    ("LIMIT", reasons::REASON_NOT_A_CALL, "names_Constant"),
    // Through an alias the variant is not read; only the struct `A` is refused.
    ("K::A", reasons::REASON_DECLINED_BY_SCOPE, "variant_guard"),
    // kani/orphan.rs is reached by no declaration.
    ("other", reasons::REASON_OUTSIDE_TARGETS, ""),
    // kani/proofs.rs is in the library through its #[path], not outside it.
    ("undefined_fn", reasons::REASON_UNKNOWN_CALLEE, ""),
];

#[test]
fn every_open_site_names_its_reason_and_no_resolved_site_does() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH (the Cargo facts need it)");
        return;
    }
    let (_dir, store) = resolved_fixture();
    let all = sites(&store);
    // A lone `Set::new` of the repository must not take a path into std.
    let edges = store
        .execute_query("MATCH (c:CallSite)-[]->(t) WHERE t.id = 'src/lib.rs::Set::new' RETURN c.id")
        .expect("edges");
    assert!(edges.rows.is_empty(), "{:?}", edges.rows);
    for (callee, reason, detail) in EXPECTED {
        assert_eq!(reason_of(&all, callee), (*reason, *detail), "{callee}");
    }
    let chained = all
        .iter()
        .find(|(callee, _)| callee.ends_with(".count"))
        .expect("the chained call");
    assert_eq!(chained.1 .1, reasons::REASON_NO_RECEIVER_TYPE);
    assert!(all["helper"].0, "helper resolves");
    for (callee, (resolved, reason, detail)) in &all {
        let recorded = !reason.is_empty() || !detail.is_empty();
        assert_eq!(
            *resolved, !recorded,
            "{callee}: resolved={resolved} reason={reason:?}"
        );
    }
}

#[test]
fn the_summary_partitions_the_open_sites() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH (the Cargo facts need it)");
        return;
    }
    let (_dir, store) = resolved_fixture();
    let open = sites(&store)
        .values()
        .filter(|(resolved, ..)| !resolved)
        .count() as u64;
    let summary = store.unresolved_site_summary().expect("summary");
    assert!(summary.reasons_recorded);
    assert_eq!(summary.total, open);
    assert_eq!(summary.by_reason.values().sum::<u64>(), open);
    assert!(!summary.by_reason.contains_key(reasons::REASON_NOT_RECORDED));
    assert_eq!(
        summary.sum_of(&BY_CONSTRUCTION) + summary.sum_of(&IMPROVABLE),
        open,
        "every recorded reason is in exactly one of the two groups"
    );
}

/// A site reopened with a stale reason is re-reasoned by the next resolve, and a
/// graph whose reasons were never recorded reads `not_recorded`, not an error.
#[test]
fn a_reopened_site_is_re_reasoned_and_an_unmarked_graph_reads_not_recorded() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH (the Cargo facts need it)");
        return;
    }
    let (_dir, store) = resolved_fixture();
    store
        .execute_query(
            "MATCH (cs:CallSite) WHERE cs.callee_name = 'nothing_here' \
             SET cs.unresolved_reason = 'cfg_twins', cs.unresolved_detail = 'stale'",
        )
        .expect("stale reason");
    crate::resolver::resolve_graph(&store).expect("resolve again");
    let all = sites(&store);
    assert_eq!(
        reason_of(&all, "nothing_here"),
        (reasons::REASON_UNKNOWN_CALLEE, "")
    );

    store
        .execute_query("MATCH (m:GraphMarker {id: 'callsite_reason_form'}) DELETE m")
        .expect("drop marker");
    store
        .execute_query(
            "MATCH (cs:CallSite) WHERE cs.callee_name = 'nothing_here' \
             SET cs.unresolved_reason = ''",
        )
        .expect("blank reason");
    let summary = store.unresolved_site_summary().expect("an old graph reads");
    assert!(!summary.reasons_recorded);
    assert_eq!(
        summary.by_reason.get(reasons::REASON_NOT_RECORDED),
        Some(&1)
    );
}
