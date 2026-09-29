// c_include_visibility_404 — issue #404. A `static` function is named by the
// file that defines it and by every file that includes it, directly or through
// another header, whatever the extension of the included file (a unity build
// includes `.c` files). The rule read the extension of the file that holds the
// function instead of the caller's `#include` directives: a `static` in a `.c`
// was never nameable from an includer, and a `static inline` in any header
// was nameable from files that never include it.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
use std::path::Path;
mod common;
use common::TempDirExt;

fn index_and_resolve(files: &[(&str, &str)]) -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("c_include_404_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture");
    for (path, body) in files {
        let p = src.join(path);
        fs::create_dir_all(p.parent().unwrap_or(Path::new("."))).expect("mkdir");
        fs::write(p, body).expect("write fixture");
    }
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    (store, tmp)
}

/// (callee, target id or "", "reason/detail") for every call site in `file`.
fn sites_in(store: &GraphStore, file: &str) -> Vec<(String, String, String)> {
    let q = format!(
        "MATCH (cs:CallSite) WHERE cs.id STARTS WITH '{file}::' \
         OPTIONAL MATCH (cs)-[:Calls_CallSite_Function]->(f) \
         RETURN cs.callee_name, f.id, cs.unresolved_reason, cs.unresolved_detail ORDER BY cs.line"
    );
    store
        .execute_query(&q)
        .expect("query sites")
        .rows
        .into_iter()
        .map(|r| {
            // An OPTIONAL MATCH with no target reads back as a null.
            let target = if r[1].starts_with("Null(") {
                String::new()
            } else {
                r[1].clone()
            };
            (r[0].clone(), target, format!("{}/{}", r[2], r[3]))
        })
        .collect()
}

fn target_of(store: &GraphStore, file: &str, callee: &str) -> (String, String) {
    let site = sites_in(store, file)
        .into_iter()
        .find(|s| s.0 == callee)
        .unwrap_or_else(|| panic!("no {callee} site in {file}"));
    (site.1, site.2)
}

#[test]
fn a_static_in_an_included_c_file_is_the_target_of_a_unity_build() {
    let imp = "static int helper(void) { return 1; }\nint impl_entry(void) { return helper(); }\n";
    let main = "#include \"impl.c\"\nint main_entry(void) { return helper(); }\n";
    let (store, _tmp) = index_and_resolve(&[("impl.c", imp), ("main.c", main)]);
    let (target, why) = target_of(&store, "main.c", "helper");
    assert!(
        target.starts_with("impl.c::helper"),
        "main.c includes impl.c, so it names impl.c's static: {target:?} {why:?}"
    );
}

#[test]
fn a_static_in_a_c_file_nobody_includes_stays_declined() {
    let imp = "static int helper(void) { return 1; }\nint impl_entry(void) { return helper(); }\n";
    let other = "int other_entry(void) { return helper(); }\n";
    let (store, _tmp) = index_and_resolve(&[("impl.c", imp), ("other.c", other)]);
    let (target, why) = target_of(&store, "other.c", "helper");
    assert_eq!(target, "", "other.c does not include impl.c");
    assert_eq!(why, "declined_by_scope/file_local");
}

#[test]
fn a_header_static_of_one_name_resolves_per_including_file() {
    let a = "static inline int clamp(int x) { return x; }\n";
    let b = "static inline int clamp(int x) { return x + 1; }\n";
    let use_a = "#include \"a.h\"\nint f(void) { return clamp(1); }\n";
    let use_b = "#include \"b.h\"\nint g(void) { return clamp(2); }\n";
    let (store, _tmp) = index_and_resolve(&[
        ("a.h", a),
        ("b.h", b),
        ("use_a.c", use_a),
        ("use_b.c", use_b),
    ]);
    let (ta, why_a) = target_of(&store, "use_a.c", "clamp");
    let (tb, why_b) = target_of(&store, "use_b.c", "clamp");
    assert!(
        ta.starts_with("a.h::clamp"),
        "use_a.c includes a.h: {ta:?} {why_a:?}"
    );
    assert!(
        tb.starts_with("b.h::clamp"),
        "use_b.c includes b.h: {tb:?} {why_b:?}"
    );
}

#[test]
fn a_header_static_is_not_named_by_a_file_that_does_not_include_it() {
    let a = "static inline int clamp(int x) { return x; }\n";
    let user = "int f(void) { return clamp(1); }\n";
    let (store, _tmp) = index_and_resolve(&[("a.h", a), ("user.c", user)]);
    let (target, why) = target_of(&store, "user.c", "clamp");
    assert_eq!(target, "", "user.c never includes a.h");
    assert_eq!(why, "declined_by_scope/file_local");
}

#[test]
fn a_header_static_is_named_through_a_chain_of_includes() {
    let leaf = "static inline int clamp(int x) { return x; }\n";
    let mid = "#include \"leaf.h\"\n";
    let top = "#include \"mid.h\"\nint f(void) { return clamp(1); }\n";
    let (store, _tmp) = index_and_resolve(&[("leaf.h", leaf), ("mid.h", mid), ("top.c", top)]);
    let (target, why) = target_of(&store, "top.c", "clamp");
    assert!(
        target.starts_with("leaf.h::clamp"),
        "top.c reaches leaf.h through mid.h: {target:?} {why:?}"
    );
}

#[test]
fn an_include_path_with_directories_reaches_the_file_it_names() {
    let inc = "static inline int clamp(int x) { return x; }\n";
    let other = "static inline int clamp(int x) { return x + 1; }\n";
    let user = "#include \"util/clamp.h\"\nint f(void) { return clamp(1); }\n";
    let (store, _tmp) = index_and_resolve(&[
        ("include/util/clamp.h", inc),
        ("port/util/clamp2.h", other),
        ("src/user.c", user),
    ]);
    let (target, why) = target_of(&store, "src/user.c", "clamp");
    assert!(
        target.starts_with("include/util/clamp.h::clamp"),
        "the path util/clamp.h names include/util/clamp.h only: {target:?} {why:?}"
    );
}
