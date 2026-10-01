// cpp_macro_scope_410 — issue #410. In a header that mixes specifier macros
// (`ETL_NOEXCEPT`, `ETL_OVERRIDE`, `ETL_CONSTANT`, ...) and `#if` blocks,
// tree-sitter-cpp gave up on the whole file: the namespace and the class scope
// were lost, `etl::string` was not a `Struct`, and every `c_str` call on such a
// receiver stayed open for want of a class. The shapes below are those of
// ETLCPP 7d604f2e `string.h`.
//
// A call binds through the same path as #406: only through a receiver whose
// class the parser read. A receiver of a class the repository does not hold
// stays open.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
mod common;
use common::TempDirExt;

const HEADER: &str = "\
namespace etl
{
#if ETL_USING_CPP11
  inline namespace literals
  {
    inline constexpr int sv(const char* str, unsigned length) ETL_NOEXCEPT
    {
      return 1;
    }
  }
#endif

  class istring
  {
  public:
    int size() const { return 0; }
  };

  template <unsigned MAX_SIZE_>
  class string : public istring
  {
  public:
    static ETL_CONSTANT unsigned MAX_SIZE = MAX_SIZE_;

    ETL_EXPLICIT_STRING_FROM_CHAR string(const char* text)
      : istring()
    {
    }

    const char* c_str() const ETL_NOEXCEPT
    {
      return 0;
    }

    bool is_truncated() const ETL_NOEXCEPT
    {
      return false;
    }

#if ETL_HAS_ISTRING_REPAIR
    virtual void repair() ETL_OVERRIDE
#else
    void repair()
#endif
    {
    }
  };

  class string_ext : public istring
  {
  public:
    const char* c_str() const ETL_NOEXCEPT
    {
      return 0;
    }
  };
}

int use_strings(etl::string<4>& s, etl::string_ext& e, Unknown& u)
{
  return s.c_str()[0] + e.c_str()[0] + u.c_str()[0];
}
";

fn index_and_resolve(src_text: &str) -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("cpp_macro_scope_410_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture");
    fs::create_dir_all(&src).expect("mkdir");
    fs::write(src.join("string.h"), src_text).expect("write fixture");
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    (store, tmp)
}

fn struct_names(store: &GraphStore) -> Vec<String> {
    store
        .execute_query("MATCH (n:Struct) RETURN n.qualified_name")
        .expect("query structs")
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect()
}

#[test]
fn the_classes_after_the_macros_are_structs_in_their_namespace() {
    let (store, _tmp) = index_and_resolve(HEADER);
    let names = struct_names(&store);
    for want in ["etl::string", "etl::string_ext", "etl::istring"] {
        assert!(
            names
                .iter()
                .any(|n| n.ends_with(&format!("string.h::{want}"))),
            "{want} must be a Struct; got {names:?}"
        );
    }
}

#[test]
fn the_class_methods_belong_to_the_class_and_keep_their_names() {
    let (store, _tmp) = index_and_resolve(HEADER);
    let methods: Vec<String> = store
        .execute_query("MATCH (m:Method) RETURN m.qualified_name")
        .expect("query methods")
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    for want in [
        "etl::string::c_str",
        "etl::string::is_truncated",
        "etl::string::repair",
    ] {
        assert!(
            methods.iter().any(|m| m.contains(want)),
            "{want} must be a Method; got {methods:?}"
        );
    }
    assert!(
        !methods.iter().any(|m| m.contains("ETL_")),
        "no method may be named after a macro: {methods:?}"
    );
}

/// `c_str` on `etl::string` and `etl::string_ext` receivers binds to the method
/// of that class; the receiver of the class the repository does not hold stays
/// open.
#[test]
fn c_str_binds_through_the_receiver_class_and_stays_open_for_an_unknown_one() {
    let (store, _tmp) = index_and_resolve(HEADER);
    let rows = store
        .execute_query(
            "MATCH (cs:CallSite) WHERE cs.id CONTAINS 'use_strings#' AND cs.callee_name = 'c_str' \
             OPTIONAL MATCH (cs)-[:Calls_CallSite_Function]->(f) \
             OPTIONAL MATCH (cs)-[:Calls_CallSite_Method]->(m) \
             RETURN cs.id, m.id",
        )
        .expect("query sites")
        .rows;
    assert_eq!(rows.len(), 3, "three c_str sites: {rows:?}");
    let bound: Vec<&String> = rows
        .iter()
        .map(|r| &r[1])
        .filter(|t| !t.starts_with("Null("))
        .collect();
    assert_eq!(
        bound.len(),
        2,
        "two bind, the unknown receiver stays open: {rows:?}"
    );
    assert!(bound
        .iter()
        .any(|t| t.contains("string::c_str#") && !t.contains("string_ext")));
    assert!(bound.iter().any(|t| t.contains("string_ext::c_str#")));
}

/// A clean header is untouched: no rewrite, same structs.
#[test]
fn a_header_without_macros_is_indexed_as_written() {
    let clean = "namespace n { class a { public: int f() const { return 1; } }; }\n";
    let (store, _tmp) = index_and_resolve(clean);
    let names = struct_names(&store);
    assert!(
        names.iter().any(|n| n.ends_with("string.h::n::a")),
        "{names:?}"
    );
}
