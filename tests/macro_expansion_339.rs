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

/// A user type named like a std one must not get the std target.
const USER_TYPE: &str = "use std::io::Write;
pub struct Formatter;
impl Formatter {
    pub fn write_str(&mut self, _s: &str) {}
}
pub fn go(f: &mut Formatter) {
    write!(f, \"x\").ok();
}
";

/// Only `io::Write` is imported, but the destination is a field holding a
/// `fmt::Formatter`: the import says nothing about it.
const FIELD: &str = "use std::fmt;
use std::io::Write;
pub struct S<'a> {
    f: &'a mut fmt::Formatter<'a>,
}
impl<'a> S<'a> {
    pub fn go(&mut self) {
        write!(self.f, \"x\").ok();
    }
}
";

/// Two destinations of different types in one caller.
const TWO: &str = "use std::fmt;
use std::io::BufWriter;
pub fn both(f: &mut fmt::Formatter<'_>, w: &mut BufWriter<Vec<u8>>) {
    write!(f, \"a\").ok();
    write!(w, \"b\").ok();
}
";

/// Ruby keeps the `!` of `save!` in the callee name.
const RUBY: &str = "class U
  def save!
    1
  end

  def go
    self.save!
  end
end
";

struct Row {
    callee: String,
    line: u64,
    target: String,
    method: String,
    confidence: f64,
}

fn index_and_resolve() -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    index_files(&[
        ("lib.rs", LIB),
        ("amb.rs", AMBIGUOUS),
        ("field.rs", FIELD),
        ("two.rs", TWO),
        ("bang.rb", RUBY),
    ])
}

/// A graph of its own: the user-type check is by name across the repository,
/// so the user-defined `Formatter` must not share a graph with the std one.
fn index_user_type() -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    index_files(&[("user.rs", USER_TYPE)])
}

fn index_files(
    files: &[(&str, &str)],
) -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("macro_expansion_339_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture/src");
    fs::create_dir_all(&src).expect("mkdir src");
    for (name, text) in files {
        fs::write(src.join(name), text).expect("write fixture file");
    }
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

fn reason_of(res: &resolver::ResolutionResult, file: &str) -> Option<String> {
    res.unresolved
        .iter()
        .find(|u| u.from_id.starts_with(file) && u.target_text == "write!")
        .map(|u| u.reason.clone())
}

#[test]
fn a_user_type_named_formatter_never_gets_the_std_target() {
    let (store, res, _tmp) = index_user_type();
    assert!(macro_rows(&store, "src/user.rs").is_empty());
    assert!(!is_resolved(&store, "src/user.rs", 7));
    let reason = reason_of(&res, "src/user.rs").expect("reported unresolved");
    assert!(reason.starts_with("ambiguous"), "{reason}");
}

#[test]
fn write_on_a_formatter_field_is_not_decided_by_an_unrelated_import() {
    let (store, res, _tmp) = index_and_resolve();
    assert!(
        macro_rows(&store, "src/field.rs").is_empty(),
        "io::Write is imported but the destination is a Formatter field"
    );
    assert!(!is_resolved(&store, "src/field.rs", 8));
    assert!(reason_of(&res, "src/field.rs").is_some());
}

#[test]
fn two_writes_in_one_function_keep_their_own_targets() {
    let (store, _res, _tmp) = index_and_resolve();
    let rows = macro_rows(&store, "src/two.rs");
    let a = rows_on_line(&rows, 4);
    let b = rows_on_line(&rows, 5);
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_eq!(a[0].target, "core::fmt::Formatter::write_fmt");
    assert_eq!(b[0].target, "std::io::Write::write_fmt");
}

/// A Ruby `self.save!` is a method call. The macro filter used to take it for
/// a Rust macro: the plain call phase skipped it, the macro pass reported it
/// as an unknown macro, and the macro reset cleared its `is_resolved`.
#[test]
fn a_ruby_bang_call_is_never_handled_as_a_rust_macro() {
    let (store, first, _tmp) = index_and_resolve();
    let macro_reasons = [
        "no macro-expansion table entry",
        "expansion has no emit_calls entries",
        "no stable target for this expansion",
    ];
    let hit = first
        .unresolved
        .iter()
        .filter(|u| u.from_id.starts_with("src/bang.rb"))
        .find(|u| macro_reasons.contains(&u.reason.as_str()) || u.reason.starts_with("ambiguous"));
    assert!(
        hit.is_none(),
        "a Ruby call got a macro reason: {:?}",
        hit.map(|u| u.reason.clone())
    );

    let before = is_resolved(&store, "src/bang.rb", 7);
    let second = resolver::resolve_graph(&store).expect("second resolve");
    assert_eq!(second.total_refs, first.total_refs);
    assert_eq!(is_resolved(&store, "src/bang.rb", 7), before);
}

/// A `File`, `Sink` and `Cursor` that come from other crates: not std, though
/// `io::Write` is imported and the names match std types.
const EXTERNAL: &str = "use futures::Sink;
use my_crate::io::Cursor;
use std::io::Write;
use tokio::fs::File;

pub fn a(mut f: File) {
    write!(f, \"x\").ok();
}

pub fn b(mut s: Sink) {
    write!(s, \"x\").ok();
}

pub fn c(mut c: Cursor<Vec<u8>>) {
    write!(c, \"x\").ok();
}
";

/// A `Formatter` of the repository, in a file of its own.
const OWN_FORMATTER: &str = "pub struct Formatter;
pub fn go(f: &mut Formatter) {
    write!(f, \"x\").ok();
}
";

/// The std `Formatter`, in another file of the same repository.
const STD_FORMATTER: &str = "use std::fmt;
pub fn go(f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, \"x\")
}
";

/// Std types named by an imported name and by a written path.
const STD_NAMED: &str = "use std::fs::File;
pub fn a(mut f: File) {
    write!(f, \"x\").ok();
}

pub fn b(mut f: std::fs::File) {
    write!(f, \"x\").ok();
}
";

/// A bare `File` that only a glob import of an unknown crate could provide.
const GLOB_UNKNOWN: &str = "use some_crate::*;
pub fn a(mut f: File) {
    write!(f, \"x\").ok();
}
";

fn index_scope() -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    index_files(&[
        ("external.rs", EXTERNAL),
        ("own_fmt.rs", OWN_FORMATTER),
        ("std_fmt.rs", STD_FORMATTER),
        ("std_named.rs", STD_NAMED),
        ("glob.rs", GLOB_UNKNOWN),
    ])
}

#[test]
fn a_std_named_type_from_another_crate_gets_no_std_target() {
    let (store, res, _tmp) = index_scope();
    assert!(
        macro_rows(&store, "src/external.rs").is_empty(),
        "tokio File, futures Sink and an external Cursor are not std"
    );
    for line in [7, 11, 15] {
        assert!(!is_resolved(&store, "src/external.rs", line), "line {line}");
    }
    assert!(reason_of(&res, "src/external.rs").is_some());
}

#[test]
fn a_formatter_of_the_repository_does_not_decide_the_std_formatter_of_another_file() {
    let (store, _res, _tmp) = index_scope();
    assert!(macro_rows(&store, "src/own_fmt.rs").is_empty());
    let std_rows = macro_rows(&store, "src/std_fmt.rs");
    assert_eq!(std_rows.len(), 1, "the std Formatter keeps its target");
    assert_eq!(std_rows[0].target, "core::fmt::Formatter::write_fmt");
}

#[test]
fn a_std_type_named_by_import_or_by_path_resolves_to_std() {
    let (store, _res, _tmp) = index_scope();
    let rows = macro_rows(&store, "src/std_named.rs");
    let imported = rows_on_line(&rows, 3);
    let by_path = rows_on_line(&rows, 7);
    assert_eq!(imported.len(), 1, "use std::fs::File");
    assert_eq!(by_path.len(), 1, "std::fs::File written in full");
    assert_eq!(imported[0].target, "std::io::Write::write_fmt");
    assert_eq!(by_path[0].target, "std::io::Write::write_fmt");
    assert_eq!(imported[0].method, "macro-expansion-receiver-type");
}

#[test]
fn a_bare_std_name_that_only_a_glob_of_an_unknown_crate_could_provide_is_undetermined() {
    let (store, _res, _tmp) = index_scope();
    assert!(macro_rows(&store, "src/glob.rs").is_empty());
    assert!(!is_resolved(&store, "src/glob.rs", 3));
}
