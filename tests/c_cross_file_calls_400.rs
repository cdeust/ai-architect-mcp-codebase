// c_cross_file_calls_400 — issue #400. In C, a call to a function defined in
// another file goes through a header prototype. The parser emits that
// prototype as a `Function` too, so the resolver saw two candidates (the
// prototype in the header, the definition in the .c file) and left the call
// open as ambiguous: FreeRTOS `pvPortMalloc` resolved only from its own file.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
use std::path::Path;
mod common;
use common::TempDirExt;

fn index_and_resolve(files: &[(&str, &str)]) -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("c_cross_file_400_")
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

const HEADER: &str =
    "#ifndef A_H\n#define A_H\nvoid *alloc_block(int n);\nint only_declared(void);\n#endif\n";
const DEFINITION: &str = "#include \"a.h\"\nvoid *alloc_block(int n) { return 0; }\n";
const CALLER: &str =
    "#include \"a.h\"\nvoid task(void) {\n    alloc_block(4);\n    only_declared();\n}\n";

#[test]
fn a_call_through_a_header_prototype_resolves_to_the_definition() {
    let (store, _tmp) = index_and_resolve(&[("a.h", HEADER), ("a.c", DEFINITION), ("b.c", CALLER)]);
    let sites = sites_in(&store, "b.c");
    let alloc = sites
        .iter()
        .find(|s| s.0 == "alloc_block")
        .expect("alloc_block site");
    assert!(
        alloc.1.starts_with("a.c::alloc_block"),
        "the call must bind to the definition in a.c, got {alloc:?}"
    );
}

#[test]
fn a_call_with_only_a_declaration_stays_open_with_a_reason() {
    let (store, _tmp) = index_and_resolve(&[("a.h", HEADER), ("a.c", DEFINITION), ("b.c", CALLER)]);
    let sites = sites_in(&store, "b.c");
    let decl = sites
        .iter()
        .find(|s| s.0 == "only_declared")
        .expect("only_declared site");
    assert_eq!(decl.1, "", "a prototype is never a call target: {decl:?}");
    assert_eq!(
        decl.2, "not_found/declaration_only",
        "an open site names why: {decl:?}"
    );
}

/// A function-like macro and a body of one name are alternatives the build
/// chooses between (usually under exclusive `#if` configurations): no tier
/// decides, so the site stays open. Only the prototype is dropped, which the
/// candidate count shows.
#[test]
fn a_macro_and_a_body_of_one_name_stay_open_without_the_prototype() {
    let header = "#define compute(x) compute_impl(x)\nint compute(int x);\n";
    let def = "#include \"m.h\"\nint compute(int x) { return x; }\n";
    let caller = "#include \"m.h\"\nint use_it(void) { return compute(2); }\n";
    let (store, _tmp) = index_and_resolve(&[("m.h", header), ("m.c", def), ("u.c", caller)]);
    let sites = sites_in(&store, "u.c");
    let call = sites
        .iter()
        .find(|s| s.0 == "compute")
        .expect("compute site");
    assert_eq!(
        call.1, "",
        "the build decides between macro and body: {call:?}"
    );
    assert_eq!(
        call.2, "ambiguous_candidates/2",
        "macro and body, no prototype: {call:?}"
    );
}

#[test]
fn a_static_function_in_another_file_is_never_a_target() {
    let one = "static int helper(void) { return 1; }\nint one(void) { return helper(); }\n";
    let two = "static int helper(void) { return 2; }\nint two(void) { return helper(); }\n";
    let three = "int three(void) { return helper(); }\n";
    let (store, _tmp) = index_and_resolve(&[("one.c", one), ("two.c", two), ("three.c", three)]);
    let s1 = sites_in(&store, "one.c");
    let s2 = sites_in(&store, "two.c");
    assert!(
        s1[0].1.starts_with("one.c::helper"),
        "one.c calls its own static: {s1:?}"
    );
    assert!(
        s2[0].1.starts_with("two.c::helper"),
        "two.c calls its own static: {s2:?}"
    );
    let s3 = sites_in(&store, "three.c");
    assert_eq!(
        s3[0].1, "",
        "a static in another file is invisible to three.c: {s3:?}"
    );
    assert_eq!(s3[0].2, "declined_by_scope/file_local", "{s3:?}");
}

#[test]
fn every_function_records_its_body_kind() {
    let (store, _tmp) = index_and_resolve(&[("a.h", HEADER), ("a.c", DEFINITION), ("b.c", CALLER)]);
    let rows = store
        .execute_query("MATCH (f:Function) RETURN f.name, f.body_kind ORDER BY f.id")
        .expect("query body kinds")
        .rows;
    let kind = |id_prefix: &str, name: &str| {
        let q = format!(
            "MATCH (f:Function) WHERE f.id STARTS WITH '{id_prefix}' AND f.name = '{name}' RETURN f.body_kind"
        );
        store.execute_query(&q).expect("query").rows[0][0].clone()
    };
    assert!(!rows.is_empty());
    assert_eq!(kind("a.h::", "alloc_block"), "prototype");
    assert_eq!(kind("a.c::", "alloc_block"), "body");
    assert_eq!(kind("b.c::", "task"), "body");
}

#[test]
fn a_static_inline_function_in_a_header_is_named_by_its_includers() {
    let header = "static inline int twice(int x) { return 2 * x; }\n";
    let caller = "#include \"t.h\"\nint go(void) { return twice(3); }\n";
    let (store, _tmp) = index_and_resolve(&[("t.h", header), ("go.c", caller)]);
    let sites = sites_in(&store, "go.c");
    assert!(sites[0].1.starts_with("t.h::twice"), "{sites:?}");
}

#[test]
fn a_full_index_writes_the_body_kind_marker_and_tags_statics_and_macros() {
    let src = "#define twice(x) ((x) * 2)\nstatic int hidden(void) { return 1; }\nint shown(void) { return hidden(); }\n";
    let (store, _tmp) = index_and_resolve(&[("k.c", src)]);
    let rows = store
        .execute_query("MATCH (f:Function) RETURN f.name, f.body_kind, f.linkage ORDER BY f.name")
        .expect("query functions")
        .rows;
    let row = |n: &str| rows.iter().find(|r| r[0] == n).cloned().expect(n);
    assert_eq!(row("twice")[1], "macro");
    assert_eq!(
        row("hidden")[1..],
        ["body".to_string(), "internal".to_string()]
    );
    assert_eq!(row("shown")[1..], ["body".to_string(), String::new()]);
    assert!(
        store.has_body_kind(),
        "a full index writes the body_kind marker"
    );
}

/// A graph without the `body_kind_form` marker (written before #400, or by a run
/// that failed midway) is refused, so unchanged files never keep a stale `''`.
#[test]
fn a_graph_without_the_body_kind_marker_is_refused() {
    let (store, tmp) = index_and_resolve(&[("a.h", HEADER), ("a.c", DEFINITION)]);
    store
        .execute_query("MATCH (m:GraphMarker {id: 'body_kind_form'}) DELETE m")
        .expect("spoil the marker");
    assert!(!store.has_body_kind());
    drop(store);
    let Err(refused) =
        indexer::index_codebase(&tmp.path().join("fixture"), &tmp.path().join("graph"))
    else {
        panic!("an old graph must be refused");
    };
    assert!(
        refused.contains("body_kind") && refused.contains("full reindex required"),
        "{refused}"
    );
}

/// C11 §6.2.2p4: a function first declared `static` keeps internal linkage when
/// its definition does not repeat the keyword. Only its own file can name it.
const STATIC_FIRST: &str = "static void f(void);\nvoid f(void) { }\nvoid own(void) { f(); }\n";
const OTHER_CALLER: &str = "void other(void) { f(); }\n";

#[test]
fn a_static_prototype_keeps_its_later_definition_file_local() {
    let (store, _tmp) = index_and_resolve(&[
        ("a.c", STATIC_FIRST),
        ("c.c", OTHER_CALLER),
        ("d.c", "void unrelated(void) { }\n"),
    ]);
    let own = sites_in(&store, "a.c");
    assert!(
        own[0].1.starts_with("a.c::f"),
        "a.c calls its own definition: {own:?}"
    );
    let other = sites_in(&store, "c.c");
    assert_eq!(
        other[0].1, "",
        "c.c cannot name a.c's internal f: {other:?}"
    );
    assert_eq!(other[0].2, "declined_by_scope/file_local", "{other:?}");
    let linkage = store
        .execute_query(
            "MATCH (f:Function) WHERE f.id STARTS WITH 'a.c::f' RETURN f.body_kind, f.linkage ORDER BY f.id",
        )
        .expect("query a.c::f")
        .rows;
    assert_eq!(
        linkage,
        vec![
            vec!["prototype".to_string(), "internal".to_string()],
            vec!["body".to_string(), "internal".to_string()],
        ],
        "both declarations of f carry the internal linkage"
    );
}

/// With a public `f` elsewhere, the other file's call reaches that one, never
/// the internal definition in a.c.
#[test]
fn a_call_reaches_the_public_namesake_not_the_file_local_definition() {
    let (store, _tmp) = index_and_resolve(&[
        ("a.c", STATIC_FIRST),
        ("c.c", OTHER_CALLER),
        ("d.c", "void f(void) { }\n"),
    ]);
    let other = sites_in(&store, "c.c");
    assert!(
        other[0].1.starts_with("d.c::f"),
        "c.c binds the external f of d.c: {other:?}"
    );
}

/// The reverse order, `void f(void);` then `static void f(void) { }`, is
/// undefined in C (§6.2.2p7) and rejected by compilers; the file-local reading
/// is still the safe one, since it can only withhold an edge.
#[test]
fn a_static_definition_after_a_plain_prototype_is_file_local_too() {
    let reverse = "void f(void);\nstatic void f(void) { }\nvoid own(void) { f(); }\n";
    let (store, _tmp) = index_and_resolve(&[
        ("a.c", reverse),
        ("c.c", OTHER_CALLER),
        ("d.c", "void unrelated(void) { }\n"),
    ]);
    let other = sites_in(&store, "c.c");
    assert_eq!(other[0].1, "", "{other:?}");
    assert_eq!(other[0].2, "declined_by_scope/file_local", "{other:?}");
}

/// One header prototype and three real bodies (FreeRTOS heap_1..heap_3 each
/// define pvPortMalloc): dropping the prototype leaves three bodies, and the
/// build decides between them, so the call stays open at three.
#[test]
fn a_prototype_and_three_bodies_stay_ambiguous_at_three() {
    let header = "void *grab(int n);\n";
    let body = "#include \"g.h\"\nvoid *grab(int n) { return 0; }\n";
    let caller = "#include \"g.h\"\nvoid t(void) { grab(1); }\n";
    let (store, _tmp) = index_and_resolve(&[
        ("g.h", header),
        ("heap_1.c", body),
        ("heap_2.c", body),
        ("heap_3.c", body),
        ("t.c", caller),
    ]);
    let sites = sites_in(&store, "t.c");
    assert_eq!(sites[0].1, "", "no body is chosen: {sites:?}");
    assert_eq!(sites[0].2, "ambiguous_candidates/3", "{sites:?}");
}

/// A file that declares `f` `static` names its own `f` and nothing else
/// (§6.2.2p3). When that definition is not in the graph (FreeRTOS
/// CodeWarrior/ColdFire_V1/port.c: a toolchain construct the grammar cannot
/// read hides it), the call stays open; it never falls back on the external
/// `f` of another file (the MSP430 port's public `prvSetupTimerInterrupt`).
#[test]
fn a_file_that_declares_f_static_never_reaches_another_files_f() {
    let own = "static void f(void);\nvoid start(void) { f(); }\n";
    let (store, _tmp) = index_and_resolve(&[("own.c", own), ("port.c", "void f(void) { }\n")]);
    let sites = sites_in(&store, "own.c");
    assert_eq!(
        sites[0].1, "",
        "port.c's f is not the f own.c names: {sites:?}"
    );
    assert_eq!(sites[0].2, "not_found/declaration_only", "{sites:?}");
}
