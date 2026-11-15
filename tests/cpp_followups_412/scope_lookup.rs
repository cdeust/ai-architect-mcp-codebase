// cpp_followups_412::scope_lookup: issue #412 point 3. The type of a parameter of a
// function defined outside any body is looked up in the scopes that enclose the
// caller, innermost first; the first scope holding a class or an alias of that exact
// path names it. Where the graph cannot show what C++ reaches, the lookup gives up
// and the path-suffix reading keeps the site open when two classes share the name.
//
// source: ISO/IEC 14882:2017 §6.4.1 [basic.lookup.unqual] ¶6 and ¶8.

use super::*;

/// The call `m` of `caller` binds `want` and nothing else.
fn assert_binds(file: &str, src: &str, caller: &str, want: &str) {
    let (store, _tmp) = index_and_resolve(&[(file, src)]);
    assert_eq!(bound_methods(&store, caller), [want], "{src}");
}

/// The call `m` of `caller` binds nothing and stays open as `ambiguous_candidates`.
fn assert_ambiguous(file: &str, src: &str, caller: &str) {
    let (store, _tmp) = index_and_resolve(&[(file, src)]);
    assert!(
        bound_methods(&store, caller).is_empty(),
        "{:?}",
        bound_methods(&store, caller)
    );
    assert_eq!(open_reasons(&store, caller), ["ambiguous_candidates"]);
}

#[test]
fn a_member_type_of_the_callers_class_hides_the_namespace_one() {
    let src = "\
namespace a {
  struct Box { int m() { return 1; } };
  struct C { struct Box { int m() { return 2; } }; int sl1(Box& b) { return b.m(); } };
}
";
    assert_binds("sl1.cpp", src, "sl1", "m -> a::C::Box::m");
}

#[test]
fn a_global_function_binds_the_global_class_beside_a_namespace_namesake() {
    let src = "\
namespace a { struct Box { int m() { return 1; } }; }
struct Box { int m() { return 2; } };
int sl2(Box& b) { return b.m(); }
";
    assert_binds("sl2.cpp", src, "sl2", "m -> Box::m");
}

#[test]
fn an_outer_namespace_names_the_class_when_the_inner_one_has_none() {
    let src = "\
struct Box { int m() { return 1; } };
namespace q { struct Box { int m() { return 2; } }; }
namespace a { namespace b { int sl3(Box& b) { return b.m(); } } }
";
    assert_binds("sl3.cpp", src, "sl3", "m -> Box::m");
}

#[test]
fn a_type_written_from_the_global_scope_is_looked_up_there_only() {
    let src = "\
struct Box { int m() { return 1; } };
namespace a { struct Box { int m() { return 2; } }; int sl4(::Box& b) { return b.m(); } }
";
    assert_binds("sl4.cpp", src, "sl4", "m -> Box::m");
}

#[test]
fn a_qualified_type_is_tried_under_each_enclosing_scope() {
    let src = "\
namespace in { struct Box { int m() { return 1; } }; }
namespace a {
  namespace in { struct Box { int m() { return 2; } }; }
  int sl5(in::Box& b) { return b.m(); }
}
";
    assert_binds("sl5.cpp", src, "sl5", "m -> a::in::Box::m");
}

#[test]
fn a_base_of_the_found_class_is_still_searched() {
    let src = "\
struct Box { int m() { return 1; } };
namespace a {
  struct Root { int m() { return 2; } };
  struct Box : Root {};
  int sl6(Box& b) { return b.m(); }
}
";
    assert_binds("sl6.cpp", src, "sl6", "m -> a::Root::m");
}

#[test]
fn a_using_directive_that_may_bring_the_name_in_keeps_the_suffix_reading() {
    let src = "\
namespace other { struct Box { int m() { return 1; } }; }
struct Box { int m() { return 2; } };
namespace a { using namespace other; int sl7(Box& b) { return b.m(); } }
";
    assert_ambiguous("sl7.cpp", src, "sl7");
}

#[test]
fn a_using_directive_of_a_namespace_without_the_name_does_not_stop_the_lookup() {
    let src = "\
namespace other { struct Other { int m() { return 1; } }; }
struct Box { int m() { return 2; } };
namespace a { struct Box { int m() { return 3; } }; using namespace other; int sl8(Box& b) { return b.m(); } }
";
    assert_binds("sl8.cpp", src, "sl8", "m -> a::Box::m");
}

#[test]
fn a_class_scope_with_bases_and_no_member_of_that_name_keeps_the_suffix_reading() {
    // `Box` in `D` may be a member type `D` inherits from a base the graph reads by
    // name only.
    let src = "\
struct Base { int z() { return 0; } };
struct Box { int m() { return 1; } };
namespace q { struct Box { int m() { return 2; } }; }
struct D : Base { int sl9(Box& b) { return b.m(); } };
";
    assert_ambiguous("sl9.cpp", src, "sl9");
}

#[test]
fn a_lambda_parameter_keeps_the_suffix_reading() {
    // A lambda's parameter type is read in the enclosing block, which may declare
    // the name without a node in the graph.
    let src = "\
struct Box { int m() { return 1; } };
namespace a { struct Box { int m() { return 2; } };
  int sl10() { auto l = [](Box& b) { return b.m(); }; return l(Box()); } }
";
    let (store, _tmp) = index_and_resolve(&[("sl10.cpp", src)]);
    assert!(
        bound_methods(&store, "sl10").is_empty(),
        "{:?}",
        bound_methods(&store, "sl10")
    );
}

#[test]
fn a_name_no_scope_holds_keeps_the_suffix_reading() {
    let src = "\
namespace p { struct Box { int m() { return 1; } }; }
namespace q { struct Box { int m() { return 2; } }; }
namespace a { int sl11(Box& b) { return b.m(); } }
";
    assert_ambiguous("sl11.cpp", src, "sl11");
}

#[test]
fn a_typedef_of_the_callers_namespace_names_the_class_it_stands_for() {
    let src = "\
struct Box { int m() { return 1; } };
namespace impl { struct Real { int m() { return 2; } }; }
namespace a { typedef impl::Real Box; int sl12(Box& b) { return b.m(); } }
";
    assert_binds("sl12.cpp", src, "sl12", "m -> impl::Real::m");
}
