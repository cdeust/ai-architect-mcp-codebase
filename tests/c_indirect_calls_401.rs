// c_indirect_calls_401 — issue #401. A C/C++ call whose callee is not a name
// (`(*fp)()`, `table[i]()`) was dropped at parse time: no `CallSite`, so
// nothing in the graph said a call happens there. A C member call
// (`s->cb()`, always a function pointer: C has no methods) was cut down to
// its field name and could bind by bare name to an unrelated repository
// function called `cb`. Both now stay open as `indirect_call`, and every call
// records the shape of its callee.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
use std::path::Path;
mod common;
use common::TempDirExt;

fn index_and_resolve(files: &[(&str, &str)]) -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("c_indirect_401_")
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

/// (callee, shape, target id or "", "reason/detail") for every call site in
/// `file`, in source order. `rel` names the per-site table to follow.
fn sites_in(store: &GraphStore, file: &str, rel: &str) -> Vec<[String; 4]> {
    let q = format!(
        "MATCH (cs:CallSite) WHERE cs.id STARTS WITH '{file}::' \
         OPTIONAL MATCH (cs)-[:{rel}]->(f) \
         RETURN cs.callee_name, cs.callee_shape, f.id, cs.unresolved_reason, \
         cs.unresolved_detail ORDER BY cs.line, cs.col"
    );
    store
        .execute_query(&q)
        .expect("query sites")
        .rows
        .into_iter()
        .map(|r| {
            // An OPTIONAL MATCH with no target reads back as a null.
            let target = if r[2].starts_with("Null(") {
                String::new()
            } else {
                r[2].clone()
            };
            [
                r[0].clone(),
                r[1].clone(),
                target,
                format!("{}/{}", r[3], r[4]),
            ]
        })
        .collect()
}

const CALLBACKS: &str = "\
struct ops { int (*cb)(int); };
int cb(int x) { return x; }
int twice(int x) { return x * 2; }
int (*table[2])(int) = { cb, twice };
int run(struct ops *s, int (*fp)(int), int i) {
    int a = (*fp)(1);
    int b = table[i](2);
    int c = s->cb(3);
    int d = cb(4);
    return a + b + c + d;
}
";

fn site<'a>(sites: &'a [[String; 4]], line_marker: &str) -> &'a [String; 4] {
    sites
        .iter()
        .find(|s| s[0] == line_marker)
        .unwrap_or_else(|| panic!("no site named {line_marker:?} in {sites:?}"))
}

#[test]
fn a_call_through_a_dereferenced_pointer_is_a_site_left_open() {
    let (store, _tmp) = index_and_resolve(&[("ops.c", CALLBACKS)]);
    let sites = sites_in(&store, "ops.c", "Calls_CallSite_Function");
    let fp = site(&sites, "(*fp)");
    assert_eq!(fp[1], "indirect", "{fp:?}");
    assert_eq!(fp[2], "", "an indirect call binds nothing: {fp:?}");
    assert_eq!(fp[3], "indirect_call/indirect", "{fp:?}");
}

#[test]
fn a_call_through_a_table_entry_is_a_site_left_open() {
    let (store, _tmp) = index_and_resolve(&[("ops.c", CALLBACKS)]);
    let sites = sites_in(&store, "ops.c", "Calls_CallSite_Function");
    let entry = site(&sites, "table[i]");
    assert_eq!(entry[1], "indirect", "{entry:?}");
    assert_eq!(entry[2], "", "{entry:?}");
    assert_eq!(entry[3], "indirect_call/indirect", "{entry:?}");
}

/// `s->cb(3)` calls whatever `s->cb` points to, not the function `cb` the
/// repository happens to define: the site stays open, while the direct call
/// `cb(4)` two lines below still binds to that function.
#[test]
fn a_c_member_call_never_binds_to_a_namesake() {
    let (store, _tmp) = index_and_resolve(&[("ops.c", CALLBACKS)]);
    let sites = sites_in(&store, "ops.c", "Calls_CallSite_Function");
    let calls: Vec<_> = sites.iter().filter(|s| s[0] == "cb").collect();
    assert_eq!(
        calls.len(),
        2,
        "one member call, one direct call: {sites:?}"
    );
    let (member, direct) = (calls[0], calls[1]);
    assert_eq!(member[1], "member", "{member:?}");
    assert_eq!(member[2], "", "s->cb() is a pointer call: {member:?}");
    assert_eq!(member[3], "indirect_call/member", "{member:?}");
    assert_eq!(direct[1], "direct", "{direct:?}");
    assert!(
        direct[2].starts_with("ops.c::cb"),
        "the direct call still binds: {direct:?}"
    );
    assert_eq!(
        direct[3], "/",
        "a resolved site carries no reason: {direct:?}"
    );
}

/// C++ member calls keep resolving: `w.run()` names a method, and the
/// repository has one `run`.
#[test]
fn a_cpp_member_call_still_resolves() {
    let source = "\
class Worker {
public:
    int run(int x) { return x; }
};
int drive(Worker &w) { return w.run(1); }
";
    let (store, _tmp) = index_and_resolve(&[("w.cpp", source)]);
    let sites = sites_in(&store, "w.cpp", "Calls_CallSite_Method");
    let run = site(&sites, "run");
    assert_eq!(run[1], "member", "{run:?}");
    assert!(
        run[2].contains("Worker::run"),
        "a C++ member call resolves to the method: {run:?}"
    );
}

#[test]
fn a_cpp_call_through_a_pointer_is_a_site_left_open() {
    let source = "int apply(int (*fp)(int)) { return (*fp)(3); }\n";
    let (store, _tmp) = index_and_resolve(&[("p.cpp", source)]);
    let sites = sites_in(&store, "p.cpp", "Calls_CallSite_Function");
    let fp = site(&sites, "(*fp)");
    assert_eq!(fp[1], "indirect", "{fp:?}");
    assert_eq!(fp[2], "", "{fp:?}");
    assert_eq!(fp[3], "indirect_call/indirect", "{fp:?}");
}
