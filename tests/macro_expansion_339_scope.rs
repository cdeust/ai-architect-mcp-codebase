// macro_expansion_339_scope — issue #339: how a `write!` destination's type
// is placed by the imports of its file: aliases, conflicting bindings,
// function-local `use`, and graphs written by an older build.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::resolver;
mod common;
mod macro_339_support;
use macro_339_support::{index_files, macro_rows, rows_on_line};

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
/// of the type. This test measures what that does to one site: the edge is
/// lost and no other target replaces it.
#[test]
fn a_graph_indexed_with_a_bare_hint_loses_the_edge_and_gains_no_false_one() {
    let (store, _res, _tmp) = index_files(&[("lib.rs", OLD_SHAPE)]);
    assert_eq!(rows_on_line(&macro_rows(&store, "src/lib.rs"), 3).len(), 1);
    store
        .execute_query(
            "MATCH (cs:CallSite) WHERE cs.callee_name = 'write!' AND cs.id STARTS WITH 'src/lib.rs' \
             SET cs.receiver_hint = 'Formatter'",
        )
        .expect("rewrite the hint as an older build stored it");
    resolver::resolve_graph(&store).expect("re-resolve");
    let rows = macro_rows(&store, "src/lib.rs");
    assert!(
        rows_on_line(&rows, 8).is_empty(),
        "bare Formatter is not placed"
    );
    assert!(
        rows.iter()
            .all(|r| r.target != "std::io::Write::write_fmt" || r.line != 3),
        "and no wrong target replaces it"
    );
}
