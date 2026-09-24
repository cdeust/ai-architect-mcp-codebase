// macro_expansion_339_scope — issue #339: how a `write!` destination's type
// is placed by the imports of its file: aliases, conflicting bindings,
// function-local `use`, and graphs written by an older build.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::resolver;
mod common;
mod macro_339_support;
use macro_339_support::{index_files, is_resolved, macro_rows, reason_on_line, rows_on_line};

/// `File` is an alias of the tokio type, `SF` of the std one, and `Custom`
/// comes from a glob of another crate; `io::Write` is imported in all three.
const ALIASED: &str = "use std::fs::File as SF;
use std::io::Write;
use tokio::fs::File as F;
use tokio::io::*;

pub fn a(f: &mut F) {
    write!(f, \"x\").ok();
}

pub fn b(f: &mut SF) {
    write!(f, \"x\").ok();
}

pub fn c(f: &mut Custom) {
    write!(f, \"x\").ok();
}
";

/// The same name bound to std and to tokio by two imports of one file.
const CONFLICT: &str = "use std::fs::File;
use std::io::Write;

mod inner {
    use tokio::fs::File;
    pub fn x(f: &mut File) {
        write!(f, \"x\").ok();
    }
}

pub fn y(f: &mut File) {
    write!(f, \"x\").ok();
}
";

/// A `use` inside the function body, which the index does not record.
const LOCAL_USE: &str = "use std::fs::File;
use std::io::Write;

pub fn a(f: &mut File) {
    use tokio::fs::File;
    write!(f, \"x\").ok();
}
";

/// One `write!` on a std `Formatter`, imported by module.
const OLD_SHAPE_IMPORTED: &str = "use std::fmt::Formatter;\npub fn go(f: &mut Formatter<'_>) -> std::fmt::Result {\n    write!(f, \"x\")\n}\n";

const OLD_SHAPE: &str = "use std::fmt;\npub fn go(f: &mut fmt::Formatter<'_>) -> fmt::Result {\n    write!(f, \"x\")\n}\n";

fn index_shadowing() -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    index_files(&[
        ("aliased.rs", ALIASED),
        ("conflict.rs", CONFLICT),
        ("local_use.rs", LOCAL_USE),
    ])
}

#[test]
fn an_alias_of_a_non_std_type_gets_no_std_target_and_a_std_alias_resolves() {
    let (store, _res, _tmp) = index_shadowing();
    let rows = macro_rows(&store, "src/aliased.rs");
    assert!(rows_on_line(&rows, 7).is_empty(), "`F` is tokio's File");
    let std_alias = rows_on_line(&rows, 11);
    assert_eq!(std_alias.len(), 1, "`SF` is std's File");
    assert_eq!(std_alias[0].target, "std::io::Write::write_fmt");
    assert_eq!(std_alias[0].method, "macro-expansion-receiver-type");
}

#[test]
fn a_named_type_that_no_import_places_is_not_decided_by_an_unrelated_write_import() {
    let (store, _res, _tmp) = index_shadowing();
    let rows = macro_rows(&store, "src/aliased.rs");
    assert!(
        rows_on_line(&rows, 15).is_empty(),
        "`Custom` may come from the tokio glob: io::Write in scope says nothing"
    );
}

#[test]
fn a_name_bound_to_std_and_to_another_crate_in_one_file_is_undetermined() {
    let (store, _res, _tmp) = index_shadowing();
    assert!(
        macro_rows(&store, "src/conflict.rs").is_empty(),
        "two imports bind `File` to different origins"
    );
}

#[test]
fn a_use_inside_the_function_body_makes_the_destination_undetermined() {
    let (store, _res, _tmp) = index_shadowing();
    assert!(
        macro_rows(&store, "src/local_use.rs").is_empty(),
        "the index does not see the function-local `use tokio::fs::File`"
    );
}

/// A graph indexed before the type path was recorded holds the last segment
/// of the type. This test measures what that does to one site: a bare
/// `Formatter` that nothing in the file places loses its edge, and the same
/// stored hint keeps it when the file imports the std type by name.
#[test]
fn a_graph_indexed_with_a_bare_hint_loses_the_edge_and_gains_no_false_one() {
    let rewrite = "MATCH (cs:CallSite) WHERE cs.callee_name = 'write!' \
                   SET cs.receiver_hint = 'Formatter'";
    let (store, _res, _tmp) = index_files(&[("lib.rs", OLD_SHAPE)]);
    assert_eq!(rows_on_line(&macro_rows(&store, "src/lib.rs"), 3).len(), 1);
    store.execute_query(rewrite).expect("store the bare hint");
    resolver::resolve_graph(&store).expect("re-resolve");
    assert!(
        rows_on_line(&macro_rows(&store, "src/lib.rs"), 3).is_empty(),
        "a bare Formatter that the file does not place is not decided"
    );

    let (store, _res, _tmp) = index_files(&[("lib.rs", OLD_SHAPE_IMPORTED)]);
    store.execute_query(rewrite).expect("store the bare hint");
    resolver::resolve_graph(&store).expect("re-resolve");
    let rows = macro_rows(&store, "src/lib.rs");
    let kept = rows_on_line(&rows, 3);
    assert_eq!(kept.len(), 1, "positive control: the import places it");
    assert_eq!(kept[0].target, "core::fmt::Formatter::write_fmt");
}

/// Types the extractor cannot name, and locals with no declared type: the
/// imported write trait says nothing about what they are.
const UNNAMEABLE: &str = "use std::fmt;
use std::io::Write;

pub fn a(w: &mut impl std::fmt::Write) {
    write!(w, \"x\").ok();
}

pub fn b(w: &mut dyn fmt::Write) {
    write!(w, \"x\").ok();
}

pub fn c<W: fmt::Write>(w: &mut W) {
    write!(w, \"x\").ok();
}

pub fn d() {
    let mut w = make_writer();
    write!(w, \"x\").ok();
}
";

/// An untyped local, `io::Write` at the top of the file and a `use` of
/// `fmt::Write` in the function body, which the index does not record.
const UNTYPED_LOCAL_USE: &str = "use std::io::Write;

pub fn a() {
    use std::fmt::Write;
    let mut w = make_writer();
    write!(w, \"x\").ok();
}
";

#[test]
fn a_destination_the_extractor_cannot_name_is_never_decided_by_an_imported_trait() {
    let (store, res, _tmp) = index_files(&[("unnameable.rs", UNNAMEABLE)]);
    let rows = macro_rows(&store, "src/unnameable.rs");
    for (line, what) in [
        (5, "impl fmt::Write"),
        (9, "dyn fmt::Write"),
        (13, "a generic parameter"),
        (18, "a local with no declaration"),
    ] {
        let got: Vec<&str> = rows_on_line(&rows, line)
            .iter()
            .map(|r| r.target.as_str())
            .collect();
        assert!(got.is_empty(), "{what} got {got:?}");
        assert!(!is_resolved(&store, "src/unnameable.rs", line), "{what}");
        assert_eq!(
            reason_on_line(&res, "src/unnameable.rs", line).as_deref(),
            Some("destination type not determined"),
            "{what}"
        );
    }
}

#[test]
fn an_untyped_local_with_a_function_local_use_is_not_decided_by_the_module_import() {
    let (store, _res, _tmp) = index_files(&[("untyped.rs", UNTYPED_LOCAL_USE)]);
    assert!(macro_rows(&store, "src/untyped.rs").is_empty());
}

/// Destinations built by a constructor of their own type, through `?`,
/// `.unwrap()` or `.expect(..)`, and two that are not knowable.
const CONSTRUCTED: &str = "use std::fs::File;
use std::io::{BufWriter, Write};

pub fn a(p: &str) -> std::io::Result<()> {
    let mut f = File::create(p)?;
    writeln!(f, \"x\")?;
    Ok(())
}

pub fn b(p: &str) -> std::io::Result<()> {
    let mut f = File::open(p).unwrap();
    write!(f, \"x\")?;
    Ok(())
}

pub fn c(p: &str) -> std::io::Result<()> {
    let mut w = BufWriter::new(File::create(p)?);
    write!(w, \"x\")?;
    Ok(())
}

pub fn d() {
    let mut s = String::new();
    write!(s, \"x\").ok();
}

pub fn e(p: &str) -> std::io::Result<()> {
    let mut f = File::create(p).expect(\"created\");
    write!(f, \"x\")?;
    Ok(())
}

pub fn n(o: Option<File>) {
    let mut f = o.map(|x| x);
    write!(f, \"x\").ok();
}

pub fn m(p: &str) -> std::io::Result<()> {
    let mut f = File::parse(p)?;
    write!(f, \"x\")?;
    Ok(())
}
";

#[test]
fn a_destination_built_by_its_own_constructor_gets_its_target() {
    let (store, _res, _tmp) = index_files(&[("built.rs", CONSTRUCTED)]);
    let rows = macro_rows(&store, "src/built.rs");
    for (line, what) in [
        (6, "File::create(p)?"),
        (12, "File::open(p).unwrap()"),
        (18, "BufWriter::new(File::create(p)?)"),
        (29, "File::create(p).expect(..)"),
    ] {
        let on = rows_on_line(&rows, line);
        let all: Vec<(u64, &str)> = rows.iter().map(|r| (r.line, r.target.as_str())).collect();
        assert_eq!(on.len(), 1, "{what}: {all:?}");
        assert_eq!(on[0].target, "std::io::Write::write_fmt", "{what}");
    }
    let string = rows_on_line(&rows, 24);
    assert_eq!(string.len(), 1, "String::new()");
    assert_eq!(string[0].target, "std::fmt::Write::write_fmt");
}

#[test]
fn a_destination_whose_type_no_constructor_tells_stays_undetermined() {
    let (store, _res, _tmp) = index_files(&[("built.rs", CONSTRUCTED)]);
    let rows = macro_rows(&store, "src/built.rs");
    assert!(
        rows_on_line(&rows, 35).is_empty(),
        "o.map(..) is not knowable"
    );
    assert!(
        rows_on_line(&rows, 40).is_empty(),
        "File::parse(p)? is not a known constructor"
    );
}

/// Wrappers that are not the constructor's own value (`.await`,
/// `.unwrap_or_default()`, `.unwrap_or_else(..)`), and two more constructors.
const CONSTRUCTED_MORE: &str = "use std::fs::File;
use std::io::Write;
use std::net::TcpStream;

pub async fn a(p: &str) -> std::io::Result<()> {
    let mut f = tokio::fs::File::create(p).await?;
    write!(f, \"x\")?;
    Ok(())
}

pub fn b(p: &str) -> std::io::Result<()> {
    let mut f = File::open(p).unwrap_or_default();
    write!(f, \"x\")?;
    Ok(())
}

pub fn c(p: &str) -> std::io::Result<()> {
    let mut f = File::open(p).unwrap_or_else(|_| panic!());
    write!(f, \"x\")?;
    Ok(())
}

pub fn d(n: usize) {
    let mut v = Vec::with_capacity(n);
    write!(v, \"x\").ok();
}

pub fn e(a: &str) -> std::io::Result<()> {
    let mut s = TcpStream::connect(a)?;
    write!(s, \"x\")?;
    Ok(())
}
";

/// The same `connect`, with a `TcpStream` that is tokio's.
const TOKIO_TCP: &str = "use tokio::net::TcpStream;

pub fn e(a: &str) -> std::io::Result<()> {
    let mut s = TcpStream::connect(a)?;
    write!(s, \"x\")?;
    Ok(())
}
";

#[test]
fn constructors_and_wrappers_outside_the_known_shapes_get_the_right_answer() {
    let (store, _res, _tmp) =
        index_files(&[("more.rs", CONSTRUCTED_MORE), ("tokio_tcp.rs", TOKIO_TCP)]);
    let rows = macro_rows(&store, "src/more.rs");
    for (line, what) in [
        (7, "tokio::fs::File::create(p).await?"),
        (13, "File::open(p).unwrap_or_default()"),
        (19, "File::open(p).unwrap_or_else(..)"),
    ] {
        assert!(
            rows_on_line(&rows, line).is_empty(),
            "{what} must get no target"
        );
    }
    for (line, what) in [
        (25, "Vec::with_capacity(n)"),
        (30, "std TcpStream::connect(a)?"),
    ] {
        let on = rows_on_line(&rows, line);
        assert_eq!(on.len(), 1, "{what}");
        assert_eq!(on[0].target, "std::io::Write::write_fmt", "{what}");
    }
    assert!(
        macro_rows(&store, "src/tokio_tcp.rs").is_empty(),
        "a tokio TcpStream is not std's"
    );
}
