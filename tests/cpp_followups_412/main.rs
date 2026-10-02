// cpp_followups_412 — issue #412, the C++ follow-ups left after #406:
//   1. a macro that closes a brace the parser cannot see (ETL's `ETL_END_ENUM_TYPE`)
//      swallowed the rest of the header into the class, so `etl::left_n` was read as
//      `string_pad_direction::left_n`;
//   2. `catch (Foo& e)` declared no name, so `e.f()` had no receiver type (and, below
//      a parameter of the same name, took that parameter's type);
//   3. a declared type was matched by path suffix, so `Box` in `a::use` named every
//      class `Box` of the repository, not the one its own scope declares. Only a
//      parameter of the function definition is read through the scopes (its type is
//      written outside every body); a receiver declared in a body keeps the suffix
//      reading, because a block may declare the type's name and the graph has no
//      node for a block.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
#[path = "../common/mod.rs"]
mod common;
use common::TempDirExt;
mod body_receivers;

fn index_and_resolve(files: &[(&str, &str)]) -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("cpp_followups_412_")
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

/// The qualified names (file prefix removed) of the nodes of `label`.
fn names_of(store: &GraphStore, label: &str) -> Vec<String> {
    let mut names: Vec<String> = store
        .execute_query(&format!("MATCH (n:{label}) RETURN n.qualified_name"))
        .expect("query names")
        .rows
        .into_iter()
        .map(|r| {
            r[0].split_once("::")
                .map_or(r[0].clone(), |(_, q)| q.to_string())
        })
        .collect();
    names.sort();
    names
}

/// The methods the call sites of the callers whose id contains `caller` bind to,
/// each as `callee -> target` (file prefix removed), sorted.
fn bound_methods(store: &GraphStore, caller: &str) -> Vec<String> {
    let mut out: Vec<String> = store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[:Calls_CallSite_Method]->(m:Method) \
             WHERE cs.id CONTAINS '{caller}#' RETURN cs.callee_name, m.qualified_name"
        ))
        .expect("query bound sites")
        .rows
        .into_iter()
        .map(|r| {
            let target = r[1].split_once("::").map_or(&r[1][..], |(_, q)| q);
            let target = target.rsplit_once('#').map_or(target, |(name, _)| name);
            format!("{} -> {target}", r[0])
        })
        .collect();
    out.sort();
    out
}

#[test]
fn a_macro_that_closes_a_hidden_brace_does_not_swallow_the_rest_of_the_header() {
    // The `typename T::size_type(0U)` cast is the localized error that, with the
    // macros, made tree-sitter give up on the whole header (ETLCPP 7d604f2e
    // `string_utilities.h`).
    let header = "\
#ifndef PAD_INCLUDED
#define PAD_INCLUDED
namespace etl
{
  struct pad_direction
  {
    enum enum_type
    {
      LEFT,
      RIGHT,
    };

    ETL_DECLARE_ENUM_TYPE(pad_direction, int)
    ETL_ENUM_TYPE(LEFT,  \"left\")
    ETL_ENUM_TYPE(RIGHT, \"right\")
    ETL_END_ENUM_TYPE
  };

  template <typename TString>
  void left_n(TString& s)
  {
    s.insert(typename TString::size_type(0U), 3, 'x');
  }
}
#endif
";
    let (store, _tmp) = index_and_resolve(&[("pad.h", header)]);
    let functions = names_of(&store, "Function");
    assert!(
        functions.iter().any(|n| n.starts_with("etl::left_n#")),
        "left_n is a function of namespace etl; functions: {functions:?}"
    );
    let structs = names_of(&store, "Struct");
    assert!(
        structs.iter().any(|n| n == "etl::pad_direction"),
        "pad_direction is a class of namespace etl; structs: {structs:?}"
    );
    let methods = names_of(&store, "Method");
    assert!(
        !methods.iter().any(|n| n.contains("left_n")),
        "left_n is no method of pad_direction; methods: {methods:?}"
    );
}

#[test]
fn a_catch_parameter_types_its_receiver() {
    let src = "\
struct Err { int code() const { return 1; } };
struct Other { int code() const { return 2; } };
int handle(Err& e) {
  try { return 0; }
  catch (Other& e) { return e.code(); }
}
";
    let (store, _tmp) = index_and_resolve(&[("c.cpp", src)]);
    assert_eq!(
        bound_methods(&store, "handle"),
        ["code -> Other::code"],
        "e is an Other inside the handler, not the Err parameter it hides"
    );
}

#[test]
fn a_declared_type_is_the_class_of_the_scope_that_declares_it() {
    let src = "\
namespace a {
  struct Box { bool empty() const { return true; } };
  bool use(Box& b) { return b.empty(); }
}
namespace b {
  struct Box { bool empty() const { return false; } };
}
";
    let (store, _tmp) = index_and_resolve(&[("boxes.cpp", src)]);
    assert_eq!(
        bound_methods(&store, "use"),
        ["empty -> a::Box::empty"],
        "Box in namespace a is a::Box, not every class named Box"
    );
}

#[test]
fn a_declared_type_no_enclosing_scope_declares_keeps_the_path_suffix_reading() {
    let src = "\
namespace a {
  bool use(Box& b) { return b.empty(); }
}
namespace shapes {
  struct Box { bool empty() const { return true; } };
}
";
    let (store, _tmp) = index_and_resolve(&[("boxes.cpp", src)]);
    assert_eq!(
        bound_methods(&store, "use"),
        ["empty -> shapes::Box::empty"],
        "a using-declaration may bring Box into a: the one class of that name still binds"
    );
}

/// The reason the call sites of the callers whose id contains `caller` stay open
/// (`CallSite.unresolved_reason`), one per site, sorted.
fn open_reasons(store: &GraphStore, caller: &str) -> Vec<String> {
    let mut out: Vec<String> = store
        .execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.id CONTAINS '{caller}#' \
             AND cs.unresolved_reason <> '' RETURN cs.unresolved_reason"
        ))
        .expect("query open sites")
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    out.sort();
    out
}

const TWO_SETS_LIB: &str = "\
namespace etl {
  struct iset { int clear() { return 1; } };
  struct imap { int clear() { return 2; } };
  struct set_ : iset {};
  struct map_ : imap {};
}
";

#[test]
fn a_typedef_of_another_file_is_not_the_one_the_calling_file_writes() {
    // ETLCPP 7d604f2e `test_reference_flat_set.cpp` / `test_reference_flat_map.cpp`:
    // each .cpp writes its own `D`; a typedef in one translation unit is invisible
    // to the other.
    let set_cpp = "#include \"lib.h\"\nnamespace { typedef etl::set_ D; }\nvoid in_set() { D d; d.clear(); }\n";
    let map_cpp = "#include \"lib.h\"\ntypedef etl::map_ D;\nvoid in_map() { D d; d.clear(); }\n";
    let (store, _tmp) = index_and_resolve(&[
        ("lib.h", TWO_SETS_LIB),
        ("set.cpp", set_cpp),
        ("map.cpp", map_cpp),
    ]);
    assert_eq!(
        bound_methods(&store, "in_set"),
        ["clear -> etl::iset::clear"]
    );
    assert_eq!(
        bound_methods(&store, "in_map"),
        ["clear -> etl::imap::clear"]
    );
}

#[test]
fn a_typedef_of_another_file_in_the_same_scope_does_not_win_over_the_callers_own() {
    // Same shape with both typedefs written at the same (global) path.
    let set_cpp = "#include \"lib.h\"\ntypedef etl::set_ D;\nvoid in_set() { D d; d.clear(); }\n";
    let map_cpp = "#include \"lib.h\"\ntypedef etl::map_ D;\nvoid in_map() { D d; d.clear(); }\n";
    let (store, _tmp) = index_and_resolve(&[
        ("lib.h", TWO_SETS_LIB),
        ("set.cpp", set_cpp),
        ("map.cpp", map_cpp),
    ]);
    assert_eq!(
        bound_methods(&store, "in_set"),
        ["clear -> etl::iset::clear"]
    );
    assert_eq!(
        bound_methods(&store, "in_map"),
        ["clear -> etl::imap::clear"]
    );
}

#[test]
fn a_member_type_a_base_declares_leaves_the_call_open() {
    // `T` in `D::f` is `Base::T` (an inherited member type), not `::T`.
    let src = "\
struct Base { struct T { int m() { return 1; } }; };
struct T { int m() { return 2; } };
struct D : Base { int fx1(T& t) { return t.m(); } };
";
    let (store, _tmp) = index_and_resolve(&[("fx1.cpp", src)]);
    assert!(
        bound_methods(&store, "fx1").is_empty(),
        "{:?}",
        bound_methods(&store, "fx1")
    );
    assert_eq!(open_reasons(&store, "fx1"), ["ambiguous_candidates"]);
}

#[test]
fn a_using_declaration_leaves_the_call_open() {
    // `using other::Box;` makes `Box` in `a` mean `other::Box`, not `::Box`.
    let src = "\
namespace other { struct Box { int m() { return 1; } }; }
struct Box { int m() { return 2; } };
namespace a { using other::Box; int fx2(Box& b) { return b.m(); } }
";
    let (store, _tmp) = index_and_resolve(&[("fx2.cpp", src)]);
    assert!(
        bound_methods(&store, "fx2").is_empty(),
        "{:?}",
        bound_methods(&store, "fx2")
    );
    assert_eq!(open_reasons(&store, "fx2"), ["ambiguous_candidates"]);
}

#[test]
fn a_template_parameter_is_no_class() {
    // `T` of `f` is the template parameter, not the class `::T` nor `n::T`.
    let src = "\
struct T { int m() { return 1; } };
namespace n { struct T { int m() { return 2; } }; }
template <class T> int fx3(T& t) { return t.m(); }
";
    let (store, _tmp) = index_and_resolve(&[("fx3.cpp", src)]);
    assert!(
        bound_methods(&store, "fx3").is_empty(),
        "{:?}",
        bound_methods(&store, "fx3")
    );
    assert_eq!(open_reasons(&store, "fx3").len(), 1);
}

#[test]
fn with_no_declaring_scope_the_call_stays_ambiguous() {
    let src = "\
namespace p { struct Box { int m() { return 1; } }; }
namespace q { struct Box { int m() { return 2; } }; }
int fx4(Box& b) { return b.m(); }
";
    let (store, _tmp) = index_and_resolve(&[("fx4.cpp", src)]);
    assert!(bound_methods(&store, "fx4").is_empty());
    assert_eq!(open_reasons(&store, "fx4"), ["ambiguous_candidates"]);
}

#[test]
fn the_class_of_the_namespace_the_caller_is_in_binds() {
    let src = "\
struct Box { int m() { return 1; } };
namespace a { struct Box { int m() { return 2; } }; int fx5(Box& b) { return b.m(); } }
";
    let (store, _tmp) = index_and_resolve(&[("fx5.cpp", src)]);
    assert_eq!(bound_methods(&store, "fx5"), ["m -> a::Box::m"]);
}

#[test]
fn a_member_type_of_the_callers_own_class_binds() {
    let src = "\
struct T { int m() { return 1; } };
struct D { struct T { int m() { return 2; } }; int fx6(T& t) { return t.m(); } };
";
    let (store, _tmp) = index_and_resolve(&[("fx6.cpp", src)]);
    assert_eq!(bound_methods(&store, "fx6"), ["m -> D::T::m"]);
}

#[test]
fn a_base_with_several_generic_arguments_is_one_known_base() {
    // ETLCPP `iunordered_map::const_iterator : public etl::iterator<tag, const T>`:
    // the comma inside the generic arguments is no separator between two bases.
    let src = "\
namespace etl {
  template <class A, class B> struct iterator {};
  struct Outer {
    struct It : public etl::iterator<int, const long> {
      bool compare(const It&) const { return true; }
      friend bool same(const It& l, const It& r) { return l.compare(r); }
    };
  };
  struct Other { struct It { bool compare(const It&) const { return false; } }; };
}
";
    let (store, _tmp) = index_and_resolve(&[("it.h", src)]);
    assert_eq!(
        bound_methods(&store, "same"),
        ["compare -> etl::Outer::It::compare"]
    );
}

#[test]
fn a_source_file_sees_the_typedef_of_the_source_file_it_includes() {
    let x = "struct S { int go() { return 1; } };\ntypedef S T;\n";
    let y = "#include \"x.cpp\"\nint via_include() { T t; return t.go(); }\n";
    let z = "int no_include() { T t; return t.go(); }\n";
    let (store, _tmp) = index_and_resolve(&[("x.cpp", x), ("y.cpp", y), ("z.cpp", z)]);
    assert_eq!(bound_methods(&store, "via_include"), ["go -> S::go"]);
    assert!(bound_methods(&store, "no_include").is_empty());
}
