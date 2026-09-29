// cpp_member_calls_406 — issue #406. A C++ member call (`p->empty()`) reached
// the resolver as the bare name `empty`, the parser having dropped its
// receiver, and bound to whichever method of that name the repository held.
// An unqualified call (`width(1U)`) bound to a method of any class, even from
// a free function. A member call now binds only through a receiver whose type
// the parser read (`this`, or a parameter or local declared with a type), and
// an unqualified call names a method only of the caller's own class or of one
// of its bases. Every other member call stays open as `no_receiver_type`.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
mod common;
use common::TempDirExt;

fn index_and_resolve(files: &[(&str, &str)]) -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("cpp_member_406_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture");
    fs::create_dir_all(&src).expect("mkdir");
    for (path, body) in files {
        fs::write(src.join(path), body).expect("write fixture");
    }
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    (store, tmp)
}

/// One call site: what it calls, the target it bound to ("" when open), and
/// `reason/detail`.
#[derive(Debug)]
struct Site {
    callee: String,
    target: String,
    why: String,
}

/// The sites written inside the function or method whose id contains `caller`.
fn sites_of(store: &GraphStore, caller: &str) -> Vec<Site> {
    let mut sites: Vec<(String, Site)> = Vec::new();
    for rel in ["Calls_CallSite_Function", "Calls_CallSite_Method"] {
        let q = format!(
            "MATCH (cs:CallSite) WHERE cs.id CONTAINS '{caller}#' \
             OPTIONAL MATCH (cs)-[:{rel}]->(f) \
             RETURN cs.id, cs.callee_name, f.id, cs.unresolved_reason, cs.unresolved_detail"
        );
        for r in store.execute_query(&q).expect("query sites").rows {
            let bound = !r[2].starts_with("Null(");
            let existing = sites.iter().position(|(id, _)| *id == r[0]);
            match existing {
                Some(i) if bound => sites[i].1.target = r[2].clone(),
                Some(_) => {}
                None => sites.push((
                    r[0].clone(),
                    Site {
                        callee: r[1].clone(),
                        target: if bound { r[2].clone() } else { String::new() },
                        why: format!("{}/{}", r[3], r[4]),
                    },
                )),
            }
        }
    }
    sites.sort_by(|a, b| a.0.cmp(&b.0));
    sites.into_iter().map(|(_, s)| s).collect()
}

fn only<'a>(sites: &'a [Site], callee: &str) -> &'a Site {
    let found: Vec<&Site> = sites.iter().filter(|s| s.callee == callee).collect();
    assert_eq!(found.len(), 1, "one site named {callee:?} in {sites:?}");
    found[0]
}

const ETL: &str = "\
namespace etl {
class bloom {
public:
    int width() const { return 1; }
    bool empty() const { return false; }
    int run() { return width(); }
    int viaThis() { return this->width(); }
};
class umap {
public:
    bool empty() const { return true; }
    int span(unsigned n) { return width(n); }
    bool check(bloom* pb, umap& ru) { return pb->empty() && ru.empty(); }
    bool probe(int k) { auto pbucket = &slots[k]; return pbucket->empty(); }
    int slots[4];
};
}
int freefn(unsigned n) { return width(n); }
";

/// `pbucket->empty()`: the type of `pbucket` is `auto`, so nothing says which
/// class it is. Before the fix it bound to `umap::empty`.
#[test]
fn a_member_call_on_a_receiver_of_no_known_type_stays_open() {
    let (store, _tmp) = index_and_resolve(&[("etl.hpp", ETL)]);
    let sites = sites_of(&store, "umap::probe");
    let empty = only(&sites, "empty");
    assert_eq!(empty.target, "", "no type, no edge: {empty:?}");
    assert_eq!(empty.why, "no_receiver_type/", "{empty:?}");
}

/// `width(n)` inside `umap` names a member of `umap`, or a free function; the
/// repository holds `bloom::width` only. Before the fix it bound to it.
#[test]
fn an_unqualified_call_never_binds_a_method_of_another_class() {
    let (store, _tmp) = index_and_resolve(&[("etl.hpp", ETL)]);
    let sites = sites_of(&store, "umap::span");
    let width = only(&sites, "width");
    assert_eq!(width.target, "", "{width:?}");
    assert!(width.why.starts_with("declined_by_scope/"), "{width:?}");
}

#[test]
fn an_unqualified_call_from_a_free_function_never_binds_a_method() {
    let (store, _tmp) = index_and_resolve(&[("etl.hpp", ETL)]);
    let sites = sites_of(&store, "freefn");
    let width = only(&sites, "width");
    assert_eq!(width.target, "", "{width:?}");
    assert!(width.why.starts_with("declined_by_scope/"), "{width:?}");
}

/// Implicit `this`: `width()` in a member of `bloom` is `bloom::width`.
#[test]
fn an_unqualified_call_binds_a_method_of_the_callers_own_class() {
    let (store, _tmp) = index_and_resolve(&[("etl.hpp", ETL)]);
    let sites = sites_of(&store, "bloom::run");
    let width = only(&sites, "width");
    assert!(width.target.contains("bloom::width#"), "{width:?}");
}

#[test]
fn a_call_through_this_binds_a_method_of_the_callers_own_class() {
    let (store, _tmp) = index_and_resolve(&[("etl.hpp", ETL)]);
    let sites = sites_of(&store, "bloom::viaThis");
    let width = only(&sites, "width");
    assert!(width.target.contains("bloom::width#"), "{width:?}");
}

/// Two classes hold an `empty`; the declared type of each parameter picks its
/// own.
#[test]
fn a_member_call_on_a_typed_parameter_binds_that_types_method() {
    let (store, _tmp) = index_and_resolve(&[("etl.hpp", ETL)]);
    let sites = sites_of(&store, "umap::check");
    let targets: Vec<&str> = sites.iter().map(|s| s.target.as_str()).collect();
    assert_eq!(targets.len(), 2, "{sites:?}");
    assert!(
        targets[0].contains("bloom::empty#"),
        "pb->empty(): {sites:?}"
    );
    assert!(targets[1].contains("umap::empty#"), "ru.empty(): {sites:?}");
}

/// The definition may sit in another file than the class: the class of a
/// method is its qualified path, not its file.
#[test]
fn an_out_of_class_definition_belongs_to_its_class() {
    let header = "\
namespace etl { class bloom { public: int width() const; int other(); }; }
";
    let source = "\
#include \"b.h\"
int etl::bloom::width() const { return 1; }
int etl::bloom::other() { return width(); }
int freefn() { return width(); }
";
    let (store, _tmp) = index_and_resolve(&[("b.h", header), ("b.cpp", source)]);
    let inside = sites_of(&store, "bloom::other");
    assert!(
        only(&inside, "width").target.contains("bloom::width#1"),
        "{inside:?}"
    );
    let outside = sites_of(&store, "freefn");
    assert_eq!(only(&outside, "width").target, "", "{outside:?}");
}

/// Two overloads of one name in the class stay ambiguous: the graph holds no
/// arity, so nothing tells them apart.
#[test]
fn overloads_of_one_class_stay_ambiguous() {
    let source = "\
class two {
public:
    int f() { return 0; }
    int f(int x) { return x; }
    int g() { return f(); }
};
";
    let (store, _tmp) = index_and_resolve(&[("two.cpp", source)]);
    let sites = sites_of(&store, "two::g");
    let f = only(&sites, "f");
    assert_eq!(f.target, "", "{f:?}");
    assert_eq!(f.why, "ambiguous_candidates/2", "{f:?}");
}

/// A member of a base class is a member of the derived one, by implicit
/// `this` and through a typed receiver alike.
#[test]
fn a_method_of_a_base_class_is_reached_from_the_derived_class() {
    let source = "\
class base { public: int hello() { return 1; } };
class derived : public base {
public:
    int go() { return hello(); }
};
int outside(derived& d) { return d.hello(); }
";
    let (store, _tmp) = index_and_resolve(&[("inh.cpp", source)]);
    let inside = sites_of(&store, "derived::go");
    assert!(
        only(&inside, "hello").target.contains("base::hello#"),
        "{inside:?}"
    );
    let typed = sites_of(&store, "outside");
    assert!(
        only(&typed, "hello").target.contains("base::hello#"),
        "{typed:?}"
    );
}

/// The receiver's declared type, not the method name, chooses the target: a
/// class that lacks the method leaves the site open.
#[test]
fn a_typed_receiver_whose_class_lacks_the_method_stays_open() {
    let source = "\
class a { public: int size() { return 1; } };
class b { public: int other() { return 2; } };
int use(b& x) { return x.size(); }
";
    let (store, _tmp) = index_and_resolve(&[("ab.cpp", source)]);
    let sites = sites_of(&store, "use");
    let size = only(&sites, "size");
    assert_eq!(size.target, "", "{size:?}");
}

/// A free function called without a qualifier keeps binding.
#[test]
fn an_unqualified_call_still_binds_a_free_function() {
    let source = "\
int helper(int x) { return x; }
class k { public: int run() { return helper(1); } };
int top() { return helper(2); }
";
    let (store, _tmp) = index_and_resolve(&[("free.cpp", source)]);
    for caller in ["k::run", "top"] {
        let sites = sites_of(&store, caller);
        let helper = only(&sites, "helper");
        assert!(
            helper.target.contains("free.cpp::helper"),
            "{caller}: {helper:?}"
        );
    }
}
