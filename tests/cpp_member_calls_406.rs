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

/// A receiver declared with a `using` alias or a `typedef` has the type the
/// alias names, at any depth of aliases and through the bases of that class:
/// the graph holds a class the alias hides behind a template argument.
#[test]
fn a_receiver_declared_with_an_alias_reaches_the_class_the_alias_names() {
    let header = "\
namespace etl {
class ibasic_string { public: const char* c_str() const { return \"\"; } };
template <typename T> class basic_string_ext : public ibasic_string {};
typedef basic_string_ext<char> string_ext;
class other { public: const char* c_str() const { return \"\"; } };
}
";
    let user = "\
using Text = etl::string_ext;
using Chain = Text;
int a(Text& t) { return t.c_str() != nullptr; }
int b(Chain& t) { return t.c_str() != nullptr; }
";
    let (store, _tmp) = index_and_resolve(&[("s.h", header), ("u.cpp", user)]);
    for caller in ["u.cpp::a", "u.cpp::b"] {
        let sites = sites_of(&store, caller);
        let c_str = only(&sites, "c_str");
        assert!(
            c_str.target.contains("ibasic_string::c_str#"),
            "{caller}: {c_str:?}"
        );
    }
}

/// Two files that each define `Text` as another class: a receiver reads the
/// alias of its own file.
#[test]
fn an_alias_of_the_callers_own_file_wins_over_a_namesake_elsewhere() {
    let one = "\
class first { public: int size() { return 1; } };
using Text = first;
int f1(Text& t) { return t.size(); }
";
    let two = "\
class second { public: int size() { return 2; } };
using Text = second;
int f2(Text& t) { return t.size(); }
";
    let (store, _tmp) = index_and_resolve(&[("one.cpp", one), ("two.cpp", two)]);
    let s1 = sites_of(&store, "f1");
    assert!(only(&s1, "size").target.contains("first::size#"), "{s1:?}");
    let s2 = sites_of(&store, "f2");
    assert!(only(&s2, "size").target.contains("second::size#"), "{s2:?}");
}

/// A base whose name begins like an access specifier or `virtual` is that
/// class, not the specifier followed by the rest of the name.
#[test]
fn a_base_named_like_an_access_specifier_is_read_whole() {
    let source = "\
class public_base { public: int post() { return 1; } };
class virtual_base : public public_base {};
int use(virtual_base& v) { return v.post(); }
";
    let (store, _tmp) = index_and_resolve(&[("pub.cpp", source)]);
    let sites = sites_of(&store, "use");
    assert!(
        only(&sites, "post").target.contains("public_base::post#"),
        "{sites:?}"
    );
}

/// `q::f(x)` names a free function of the namespace `q`, not the only function
/// called `f` of another namespace: the member candidates that #406 removes
/// must not leave a namesake in a foreign namespace as the sole survivor.
#[test]
fn a_qualified_call_binds_only_a_free_function_of_the_named_namespace() {
    let source = "\
namespace etl { namespace ranges { int next(int x) { return x; } } }
namespace other { int step(int x) { return x; } }
class walker { public: int next(int x) { return x; } };
int use() {
    int a = std::next(1);
    int b = etl::ranges::next(2);
    int c = ranges::next(3);
    int d = etl::step(4);
    int e = other::step(5);
    return a + b + c + d + e;
}
";
    let (store, _tmp) = index_and_resolve(&[("ns.cpp", source)]);
    let sites = sites_of(&store, "use");
    let bound: Vec<(String, bool)> = sites
        .iter()
        .map(|s| (s.callee.clone(), !s.target.is_empty()))
        .collect();
    assert_eq!(
        bound,
        [
            ("next".into(), false),
            ("next".into(), true),
            ("next".into(), true),
            ("step".into(), false),
            ("step".into(), true),
        ],
        "{sites:?}"
    );
}
