// macro_expansion_339 — issue #339: the macro layer wrote one
// `Calls_*_StdlibSymbol` edge per expansion target of a macro call site,
// whatever its receiver, and marked the site resolved. `write!` on a
// `fmt::Formatter` also got `io::Write::write_fmt`, `writeln!` on a
// `BufWriter` also got `fmt::Write::write_fmt`, and `vec![0; n]` got three
// constructors. A macro whose target depends on its receiver or its argument
// shape now gets the one target the evidence names, or none, and a site with
// no determined target stays unresolved and says why.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
mod common;
use common::TempDirExt;

/// `io::Write` is the only write trait imported here, so a destination of
/// unknown type is decided by that import.
const LIB: &str = "use std::fmt;
use std::io::{BufWriter, Write};

pub struct P(u8);

impl fmt::Display for P {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, \"{}\", self.0)
    }
}

pub fn to_file(mut w: BufWriter<Vec<u8>>) {
    writeln!(w, \"x\").ok();
}

pub fn generic<W>(w: &mut W) {
    write!(w, \"y\").ok();
}

pub fn repeat(n: usize) -> Vec<usize> {
    vec![0usize; n]
}

pub fn empty() -> Vec<u8> {
    vec![]
}

pub fn list() -> Vec<u8> {
    vec![1, 2]
}
";

/// Both write traits are imported and the destination's type is unknown: the
/// expansion is ambiguous.
const AMBIGUOUS: &str = "use std::fmt::Write as FmtWrite;
use std::io::Write;

pub fn either<W>(w: &mut W) {
    write!(w, \"z\").ok();
}
";

struct Row {
    callee: String,
    line: u64,
    target: String,
    method: String,
    confidence: f64,
}

fn index_and_resolve() -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("macro_expansion_339_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture/src");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::write(src.join("lib.rs"), LIB).expect("write lib");
    fs::write(src.join("amb.rs"), AMBIGUOUS).expect("write amb");
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&tmp.path().join("fixture"), &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let result = resolver::resolve_graph(&store).expect("resolve");
    (store, result, tmp)
}

/// Per-site rows of the macro sites, ordered by line.
fn macro_rows(store: &GraphStore, file: &str) -> Vec<Row> {
    let qr = store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_StdlibSymbol]->(s:StdlibSymbol) \
             WHERE cs.callee_name ENDS WITH '!' AND cs.id STARTS WITH '{file}' \
             RETURN cs.callee_name, cs.line, s.canonical_path, r.resolution_method, r.confidence \
             ORDER BY cs.line"
        ))
        .expect("query rows");
    qr.rows
        .iter()
        .map(|r| Row {
            callee: r[0].clone(),
            line: r[1].parse().expect("line"),
            target: r[2].clone(),
            method: r[3].clone(),
            confidence: r[4].parse().expect("confidence"),
        })
        .collect()
}

fn is_resolved(store: &GraphStore, file: &str, line: u64) -> bool {
    let qr = store
        .execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.id STARTS WITH '{file}' AND cs.line = {line} \
             RETURN cs.is_resolved"
        ))
        .expect("query is_resolved");
    qr.rows[0][0] == "true"
}

fn rows_on_line(rows: &[Row], line: u64) -> Vec<&Row> {
    rows.iter().filter(|r| r.line == line).collect()
}

#[test]
fn write_on_a_formatter_reaches_only_the_inherent_write_fmt() {
    let (store, _res, _tmp) = index_and_resolve();
    let rows = macro_rows(&store, "src/lib.rs");
    let on = rows_on_line(&rows, 8);
    assert_eq!(on.len(), 1, "one target for write! on a Formatter");
    assert_eq!(on[0].callee, "write!");
    assert_eq!(on[0].target, "core::fmt::Formatter::write_fmt");
    assert_eq!(on[0].method, "macro-expansion-receiver-type");
    assert!((on[0].confidence - 0.8).abs() < 1e-9);
}

#[test]
fn writeln_on_a_buf_writer_never_gets_the_fmt_trait_method() {
    let (store, _res, _tmp) = index_and_resolve();
    let rows = macro_rows(&store, "src/lib.rs");
    let on = rows_on_line(&rows, 13);
    assert_eq!(on.len(), 1, "one target for writeln! on a BufWriter");
    assert_eq!(on[0].target, "std::io::Write::write_fmt");
}

#[test]
fn write_on_an_unknown_destination_is_decided_by_the_imported_trait() {
    let (store, _res, _tmp) = index_and_resolve();
    let rows = macro_rows(&store, "src/lib.rs");
    let on = rows_on_line(&rows, 17);
    assert_eq!(on.len(), 1);
    assert_eq!(on[0].target, "std::io::Write::write_fmt");
    assert_eq!(on[0].method, "macro-expansion-import-scope");
    assert!((on[0].confidence - 0.75).abs() < 1e-9);
}

#[test]
fn vec_targets_follow_the_argument_shape_and_a_list_has_none() {
    let (store, _res, _tmp) = index_and_resolve();
    let rows = macro_rows(&store, "src/lib.rs");
    let repeat = rows_on_line(&rows, 21);
    assert_eq!(repeat.len(), 1, "vec![x; n] is one call, not three");
    assert_eq!(repeat[0].target, "std::vec::from_elem");
    let empty = rows_on_line(&rows, 25);
    assert_eq!(empty.len(), 1);
    assert_eq!(empty[0].target, "std::vec::Vec::new");
    assert!(
        rows_on_line(&rows, 29).is_empty(),
        "vec![1, 2] has no stable target"
    );
    assert!(!is_resolved(&store, "src/lib.rs", 29));
    assert!(is_resolved(&store, "src/lib.rs", 21));
}

#[test]
fn an_ambiguous_write_gets_no_row_stays_unresolved_and_says_why() {
    let (store, res, _tmp) = index_and_resolve();
    assert!(macro_rows(&store, "src/amb.rs").is_empty());
    assert!(!is_resolved(&store, "src/amb.rs", 5));
    let reason = res
        .unresolved
        .iter()
        .find(|u| u.from_id.starts_with("src/amb.rs") && u.target_text == "write!")
        .map(|u| u.reason.clone())
        .expect("the ambiguous write! is reported unresolved");
    assert_eq!(reason, "ambiguous (2 candidates)");
}

#[test]
fn a_macro_row_from_an_earlier_run_is_purged_by_the_next_resolve() {
    let (store, _res, _tmp) = index_and_resolve();
    let stale = "MATCH (cs:CallSite), (s:StdlibSymbol {id: 'std::fmt::Write::write_fmt'}) \
                 WHERE cs.id STARTS WITH 'src/lib.rs' AND cs.line = 13 \
                 CREATE (cs)-[:Calls_CallSite_StdlibSymbol \
                 {confidence: 0.85, resolution_method: 'macro-expansion'}]->(s)";
    ai_architect_mcp::resolver_layers::ensure_stdlib_symbol(
        &store,
        &mut std::collections::HashSet::new(),
        "std::fmt::Write::write_fmt",
        "rust",
    )
    .expect("symbol");
    store.execute_query(stale).expect("seed a stale row");
    assert_eq!(rows_on_line(&macro_rows(&store, "src/lib.rs"), 13).len(), 2);

    resolver::resolve_graph(&store).expect("second resolve");

    let rows = macro_rows(&store, "src/lib.rs");
    let on = rows_on_line(&rows, 13);
    assert_eq!(on.len(), 1, "the stale fmt::Write row must be gone");
    assert_eq!(on[0].target, "std::io::Write::write_fmt");
}
