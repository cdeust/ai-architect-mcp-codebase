// cpp_followups_412::scope_lookup — point 3 of issue #412: a declared type
// written without a qualifier is looked up in the scopes around the caller
// (unqualified name lookup, ISO/IEC 14882:2017 §6.4.1), innermost first, instead
// of being matched by path suffix. The path-suffix reading of `main` stays where
// no scope of the caller holds a class of that name, and where a `using` the
// caller sees can make the name mean another class.

use super::*;

/// A class `Box` in `other` and one in `a`: the path suffix named both, so every
/// receiver of type `Box` kept its site open on `main`.
const TWO_BOXES: &str = "\
namespace other { struct Box { int m() { return 1; } }; }
namespace a { struct Box { int m() { return 2; } }; }
";

/// The methods the call sites of `caller` bind to, for the sources `TWO_BOXES`
/// followed by `body`.
fn bound_after_two_boxes(file: &str, body: &str, caller: &str) -> Vec<String> {
    let src = format!("{TWO_BOXES}{body}\n");
    let (store, _tmp) = index_and_resolve(&[(file, &src)]);
    bound_methods(&store, caller)
}

#[test]
fn a_parameter_of_a_namespace_function_binds_the_class_of_that_namespace() {
    assert_eq!(
        bound_after_two_boxes(
            "ns1.cpp",
            "namespace a { int fx(Box& b) { return b.m(); } }",
            "fx"
        ),
        ["m -> a::Box::m"]
    );
}

#[test]
fn a_body_receiver_of_a_namespace_function_binds_the_class_of_that_namespace() {
    assert_eq!(
        bound_after_two_boxes(
            "ns2.cpp",
            "namespace a { int fy() { Box b; return b.m(); } }",
            "fy"
        ),
        ["m -> a::Box::m"]
    );
}

#[test]
fn a_caller_at_file_scope_binds_the_class_of_the_global_scope() {
    let src = "\
namespace other { struct Box { int m() { return 1; } }; }
struct Box { int m() { return 2; } };
int g(Box& b) { return b.m(); }
";
    let (store, _tmp) = index_and_resolve(&[("g.cpp", src)]);
    assert_eq!(bound_methods(&store, "g"), ["m -> Box::m"]);
}

#[test]
fn a_method_reads_the_type_in_its_own_class_before_its_namespace() {
    let src = "\
namespace a {
  struct Box { int m() { return 1; } };
  struct C { struct Box { int m() { return 2; } }; int go(Box& b) { return b.m(); } };
}
";
    let (store, _tmp) = index_and_resolve(&[("nested.cpp", src)]);
    assert_eq!(bound_methods(&store, "go"), ["m -> a::C::Box::m"]);
}

#[test]
fn a_namesake_deeper_in_another_namespace_is_not_the_class_found() {
    let src = "\
namespace z { namespace a { struct Box { int m() { return 1; } }; } }
namespace a { struct Box { int m() { return 2; } }; int dn(Box& b) { return b.m(); } }
";
    let (store, _tmp) = index_and_resolve(&[("deep.cpp", src)]);
    assert_eq!(bound_methods(&store, "dn"), ["m -> a::Box::m"]);
}

#[test]
fn a_member_type_a_base_of_the_callers_class_declares_is_the_class_found() {
    // `T` in a method of `D : Base` is the member type `D` inherits, not `::T`.
    let src = "\
struct Base { struct T { int m() { return 1; } }; };
struct T { int m() { return 2; } };
struct D : Base { int fx(T& t) { return t.m(); } };
";
    let (store, _tmp) = index_and_resolve(&[("base.cpp", src)]);
    assert_eq!(bound_methods(&store, "fx"), ["m -> Base::T::m"]);
}

#[test]
fn a_class_of_the_caller_that_holds_no_candidate_keeps_the_path_suffix_reading() {
    // `a::Box` is what C++ names, and it has no `m`: the graph's scopes are
    // incomplete (a base it does not hold, an anonymous namespace), so the
    // path-suffix reading of `main` answers instead of declining the site.
    let src = "\
namespace other { struct Box { int m() { return 1; } }; }
namespace a { struct Box { int other_method() { return 2; } }; int fb(Box& b) { return b.m(); } }
";
    let (store, _tmp) = index_and_resolve(&[("nomethod.cpp", src)]);
    assert_eq!(bound_methods(&store, "fb"), ["m -> other::Box::m"]);
}

#[test]
fn a_using_declaration_of_a_scope_walked_leaves_the_call_open() {
    // C++ names `other::Box` for a caller in `a::b`; the graph holds no edge from
    // the using-declaration to its target, so the site stays open instead of
    // binding `a::Box`.
    let (store, _tmp) = index_and_resolve(&[(
        "ub.cpp",
        &format!(
            "{TWO_BOXES}namespace a {{ namespace b {{ using other::Box;\n\
             int ub(Box& x) {{ return x.m(); }} }} }}\n"
        ),
    )]);
    assert!(
        bound_methods(&store, "ub").is_empty(),
        "{:?}",
        bound_methods(&store, "ub")
    );
    assert_eq!(open_reasons(&store, "ub"), ["ambiguous_candidates"]);
}

#[test]
fn a_using_directive_that_can_name_the_type_leaves_the_call_open() {
    let (store, _tmp) = index_and_resolve(&[(
        "ud.cpp",
        &format!(
            "{TWO_BOXES}namespace a {{ namespace b {{ using namespace other;\n\
             int ud(Box& x) {{ return x.m(); }} }} }}\n"
        ),
    )]);
    assert!(
        bound_methods(&store, "ud").is_empty(),
        "{:?}",
        bound_methods(&store, "ud")
    );
    assert_eq!(open_reasons(&store, "ud"), ["ambiguous_candidates"]);
}

#[test]
fn a_using_declaration_of_another_name_does_not_stop_the_lookup() {
    let src = "\
namespace other { struct Box { int m() { return 1; } }; struct Other { int m() { return 3; } }; }
namespace a { struct Box { int m() { return 2; } };
  namespace b { using other::Other;
    int un(Box& x) { return x.m(); } } }
";
    let (store, _tmp) = index_and_resolve(&[("un.cpp", src)]);
    assert_eq!(bound_methods(&store, "un"), ["m -> a::Box::m"]);
}

/// The four cases of the Codex review of #542: each keeps the reading of `main`
/// (two candidates, the site open) instead of binding a class C++ does not name.
fn assert_open(file: &str, src: &str, caller: &str) {
    let (store, _tmp) = index_and_resolve(&[(file, src)]);
    assert!(
        bound_methods(&store, caller).is_empty(),
        "{:?}",
        bound_methods(&store, caller)
    );
    assert_eq!(open_reasons(&store, caller), ["ambiguous_candidates"]);
}

#[test]
fn a_directive_on_a_namespace_written_relative_to_its_scope_leaves_the_call_open() {
    assert_open(
        "rel.cpp",
        "namespace a { struct Box { int m() { return 1; } };\n\
         namespace b { namespace inner { struct Box { int m() { return 2; } }; }\n\
         using namespace inner;\nint rd(Box& x) { return x.m(); } } }\n",
        "rd",
    );
}

#[test]
fn a_base_that_names_classes_of_two_namespaces_leaves_the_call_open() {
    assert_open(
        "ub.cpp",
        "struct T { int m() { return 1; } };\n\
         namespace unrelated { struct Base { struct T { int m() { return 2; } }; }; }\n\
         namespace a { struct Base {}; struct D : Base { int bf(T& t) { return t.m(); } }; }\n",
        "bf",
    );
}

#[test]
fn an_alias_of_the_callers_namespace_leaves_the_call_open() {
    assert_open(
        "al.cpp",
        "struct Box { int m() { return 1; } };\n\
         namespace other { struct Impl { int m() { return 2; } }; }\n\
         namespace a { using Box = other::Impl; int af(Box& x) { return x.m(); } }\n",
        "af",
    );
}

#[test]
fn a_member_class_declared_after_a_method_is_not_its_parameters_type() {
    assert_open(
        "late.cpp",
        "struct Box { int m() { return 1; } };\n\
         struct C {\n  int lf(Box& b) { return b.m(); }\n  struct Box { int m() { return 2; } };\n};\n",
        "lf",
    );
}
