// cpp_followups_412 — issue #412, the C++ follow-ups left after #406:
//   1. a macro that closes a brace the parser cannot see (ETL's `ETL_END_ENUM_TYPE`)
//      swallowed the rest of the header into the class, so `etl::left_n` was read as
//      `string_pad_direction::left_n`;
//   2. `catch (Foo& e)` declared no name, so `e.f()` had no receiver type (and, below
//      a parameter of the same name, took that parameter's type);
//   3. a declared type was matched by path suffix, so `Box` in `a::use` named every
//      class `Box` of the repository, not the one its own scope declares.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
mod common;
use common::TempDirExt;

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
