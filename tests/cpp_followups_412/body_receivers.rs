// cpp_followups_412::body_receivers: a receiver declared in a function, method or
// lambda body keeps the suffix reading of `main` (issue #412), and a parameter of
// the function definition is read through the scopes around the signature.

use super::*;

/// A receiver declared in a function, method or lambda body keeps the reading of
/// `main`: its type is read by path suffix, and with two classes of that name the
/// site stays open as `ambiguous_candidates`. The body may declare the type's first
/// name itself (a using-declaration, a typedef, a local class, under a label or a
/// `case`, in the declaration of the receiver) and the graph holds no node for it:
/// nothing here tries to prove it does not.
fn assert_open_body_receiver(file: &str, src: &str, caller: &str) {
    let (store, _tmp) = index_and_resolve(&[(file, src)]);
    assert!(
        bound_methods(&store, caller).is_empty(),
        "{:?}",
        bound_methods(&store, caller)
    );
    assert_eq!(open_reasons(&store, caller), ["ambiguous_candidates"]);
}

const OTHER_BOX_AND_GLOBAL_BOX: &str = "\
namespace other { struct Box { int m() { return 1; } }; }
struct Box { int m() { return 2; } };
";

/// `OTHER_BOX_AND_GLOBAL_BOX` followed by `body`, checked open.
fn assert_open_after_preamble(file: &str, body: &str, caller: &str) {
    assert_open_body_receiver(file, &format!("{OTHER_BOX_AND_GLOBAL_BOX}{body}\n"), caller);
}

#[test]
fn a_using_declaration_in_a_function_body_leaves_the_call_open() {
    assert_open_after_preamble(
        "x1.cpp",
        "int x1() { using other::Box; Box b; return b.m(); }",
        "x1",
    );
}

#[test]
fn a_typedef_in_a_function_body_leaves_the_call_open() {
    assert_open_after_preamble(
        "x2.cpp",
        "int x2() { typedef other::Box Box; Box b; return b.m(); }",
        "x2",
    );
    assert_open_after_preamble(
        "x2b.cpp",
        "int x2b() { using Box = other::Box; Box b; return b.m(); }",
        "x2b",
    );
}

#[test]
fn a_local_class_leaves_the_call_open() {
    assert_open_after_preamble(
        "x3.cpp",
        "int x3() { struct Box { int m() { return 3; } }; Box b; return b.m(); }",
        "x3",
    );
}

#[test]
fn a_using_declaration_in_a_method_body_leaves_the_call_open() {
    assert_open_after_preamble(
        "y3.cpp",
        "struct C { int y3() { using other::Box; Box b; return b.m(); } };",
        "y3",
    );
}

#[test]
fn a_local_class_in_a_namespace_function_leaves_the_call_open() {
    let src = "\
namespace a { struct Box { int m() { return 1; } }; }
namespace b { struct Box { int m() { return 2; } }; }
namespace a { int y4() { struct Box { int m() { return 3; } }; Box x; return x.m(); } }
";
    assert_open_body_receiver("y4.cpp", src, "y4");
}

/// One test per fixture: `$test` indexes `int $name($params) { $body }` after
/// `OTHER_BOX_AND_GLOBAL_BOX` and expects the call in `$name` open.
macro_rules! body_receiver_stays_open {
    ($($test:ident: $name:literal ($params:literal) { $body:literal })+) => {$(
        #[test]
        fn $test() {
            assert_open_after_preamble(
                concat!($name, ".cpp"),
                concat!("int ", $name, "(", $params, ") { ", $body, " }"),
                $name,
            );
        }
    )+};
}

body_receiver_stays_open! {
    lb1_a_using_under_a_label_leaves_the_call_open: "lb1" ("") {
        "L: using other::Box; Box b; return b.m();" }
    lb2_a_local_class_under_a_label_leaves_the_call_open: "lb2" ("") {
        "L: struct Box { int m() { return 3; } }; Box b; return b.m();" }
    lb3_a_typedef_under_a_label_leaves_the_call_open: "lb3" ("") {
        "L: typedef other::Box Box; Box b; return b.m();" }
    lb4_a_using_under_a_label_in_a_nested_block_leaves_the_call_open: "lb4" ("") {
        "if (true) { L: using other::Box; Box b; return b.m(); } return 0;" }
    sw3_a_typedef_under_a_case_leaves_the_call_open: "sw3" ("int k") {
        "switch (k) { case 1: typedef other::Box Box; Box b; return b.m(); } return 0;" }
    sw5_a_local_class_under_a_case_leaves_the_call_open: "sw5" ("int k") {
        "switch (k) { case 1: struct Box { int m() { return 3; } }; Box b; return b.m(); } return 0;" }
    z1_a_class_defined_in_the_receiver_declaration_leaves_the_call_open: "z1" ("") {
        "struct Box { int m() { return 3; } } b; return b.m();" }
    z1b_a_class_keyword_class_in_the_receiver_declaration_leaves_the_call_open: "z1b" ("") {
        "class Box { public: int m() { return 3; } } b; return b.m();" }
    z1c_a_class_defined_in_a_pointer_receiver_declaration_leaves_the_call_open: "z1c" ("") {
        "struct Box { int m() { return 3; } } *p = nullptr; return p->m();" }
    z1d_a_class_defined_in_the_receiver_declaration_of_a_nested_block_leaves_the_call_open: "z1d" ("") {
        "if (true) { struct Box { int m() { return 3; } } b; return b.m(); } return 0;" }
}

#[test]
fn z1e_a_class_defined_in_the_receiver_declaration_in_a_namespace_leaves_the_call_open() {
    let src = "\
namespace a { struct Box { int m() { return 1; } }; }
namespace b { struct Box { int m() { return 2; } }; }
namespace a { int z1e() { struct Box { int m() { return 3; } } x; return x.m(); } }
";
    assert_open_body_receiver("z1e.cpp", src, "z1e");
}

#[test]
fn a_lambda_parameter_is_read_where_the_body_is() {
    // The parameter of a lambda is declared inside the body that may declare `Box`.
    assert_open_after_preamble(
        "l1.cpp",
        "int l1() { using other::Box; auto f = [](Box& b) { return b.m(); }; return 0; }",
        "l1",
    );
}

#[test]
fn a_declaration_of_another_name_in_a_body_keeps_the_suffix_reading() {
    // The price of failing closed: the body declares `Other`, not `Box`, and the two
    // classes named Box keep the site open, as on `main`.
    assert_open_after_preamble(
        "z1o.cpp",
        "int z1o() { struct Other { int m() { return 3; } }; Box b; return b.m(); }",
        "z1o",
    );
}

#[test]
fn a_body_receiver_of_one_class_of_that_name_binds_as_on_main() {
    let src = "struct Box { int m() { return 1; } };\nint only() { Box b; return b.m(); }\n";
    let (store, _tmp) = index_and_resolve(&[("only.cpp", src)]);
    assert_eq!(bound_methods(&store, "only"), ["m -> Box::m"]);
}

#[test]
fn a_parameter_type_is_read_in_the_scopes_around_the_signature() {
    // The signature is outside the body: a class the body declares cannot hide the
    // type the parameter is written with.
    let src = format!(
        "{OTHER_BOX_AND_GLOBAL_BOX}int p1(Box& b) {{ struct Box {{ int m() {{ return 3; }} }}; return b.m(); }}\n"
    );
    let (store, _tmp) = index_and_resolve(&[("p1.cpp", &src)]);
    assert_eq!(bound_methods(&store, "p1"), ["m -> Box::m"]);
}

#[test]
fn a_field_or_a_global_receiver_has_no_declared_type() {
    // The parser reads a type from a parameter or a body only: a field or a global
    // gives no hint, so the scope reading never applies to them and the call stays
    // open as `no_receiver_type`, as on `main`.
    let src = format!(
        "{OTHER_BOX_AND_GLOBAL_BOX}Box gb;\nstruct C {{ Box f; int fld() {{ return f.m(); }} }};\nint glob() {{ return gb.m(); }}\n"
    );
    let (store, _tmp) = index_and_resolve(&[("fg.cpp", &src)]);
    for caller in ["fld", "glob"] {
        assert!(bound_methods(&store, caller).is_empty());
        assert_eq!(open_reasons(&store, caller), ["no_receiver_type"]);
    }
}
