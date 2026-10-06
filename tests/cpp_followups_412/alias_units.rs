// cpp_followups_412::alias_units: a `using` or `typedef` that a source file writes is
// visible to a caller in another file when the two can be one translation unit: the
// caller includes the file, the file includes the caller (the alias is written, then
// the caller's header is included), or a third file includes both. Where C++ reaches
// the alias, the graph does not read which class it names, and the site stays open as
// on `main`; binding the one class named `String` (`lib::String`) is wrong.

use super::*;

const LIB_STRING: &str = "namespace lib { struct String { int m() { return 0; } }; }\n";
const A_AND_B: &str = "\
namespace cppmark {}
struct A { int m() { return 1; } };
struct B { int m() { return 2; } };
";

/// The call `s.m()` of `use` (the only caller) names no class: unbound, and open
/// because `String` may be the alias or `lib::String`.
fn assert_use_stays_open(files: &[(&str, &str)]) {
    let (store, _tmp) = index_and_resolve(files);
    assert_eq!(bound_methods(&store, "use"), Vec::<String>::new());
    assert_eq!(open_reasons(&store, "use"), ["ambiguous_candidates"]);
}

const IMPL_H: &str = "namespace cppmark {}\n#include \"s.h\"\n#include \"ab.h\"\nint use(String& s) { return s.m(); }\n";

#[test]
fn an_alias_written_before_including_the_callers_header_is_seen_by_it() {
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("impl.h", IMPL_H),
        (
            "a.cpp",
            "#include \"ab.h\"\nusing String = A;\n#include \"impl.h\"\n",
        ),
    ]);
}

#[test]
fn an_alias_of_each_of_two_files_that_include_the_callers_header_is_seen_by_it() {
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("impl.h", IMPL_H),
        (
            "a.cpp",
            "#include \"ab.h\"\nusing String = A;\n#include \"impl.h\"\n",
        ),
        (
            "b.cpp",
            "#include \"ab.h\"\nusing String = B;\n#include \"impl.h\"\n",
        ),
    ]);
}

#[test]
fn an_alias_written_before_including_a_source_file_is_seen_by_that_file() {
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("t.cpp", IMPL_H),
        (
            "u.cpp",
            "#include \"ab.h\"\nusing String = A;\n#include \"t.cpp\"\n",
        ),
    ]);
}

#[test]
fn an_alias_of_a_source_file_is_seen_by_another_source_file_a_third_includes_with_it() {
    // Unity build: `all.cpp` includes the file that writes the alias and the file
    // that uses it; neither includes the other.
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("u.cpp", "#include \"ab.h\"\nusing String = A;\n"),
        ("t.cpp", "int use(String& s) { return s.m(); }\n"),
        ("all.cpp", "#include \"u.cpp\"\n#include \"t.cpp\"\n"),
    ]);
}

#[test]
fn a_typedef_before_an_include_and_a_using_of_another_source_file_both_reach_the_header() {
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("h.h", IMPL_H),
        (
            "t.cpp",
            "#include \"ab.h\"\ntypedef A String;\n#include \"h.h\"\n",
        ),
        (
            "o.cpp",
            "#include \"s.h\"\nusing lib::String;\n#include \"h.h\"\n",
        ),
    ]);
}

#[test]
fn an_alias_of_a_source_file_no_file_includes_with_the_caller_is_not_seen() {
    // No translation unit holds both: `t.cpp` writes `using lib::String;` and is its
    // own unit, `u.cpp` is never compiled with it: `lib::String` is the only `String`.
    let (store, _tmp) = index_and_resolve(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("u.cpp", "#include \"ab.h\"\nusing String = A;\n"),
        (
            "t.cpp",
            "#include \"s.h\"\nusing lib::String;\nint use(String& s) { return s.m(); }\n",
        ),
    ]);
    assert_eq!(bound_methods(&store, "use"), ["m -> lib::String::m"]);
}

/// Glue files whose extension the language reads as C++ but the include table did not:
/// the unit that includes both the alias and the caller must still be seen.
fn glue_unit(glue: &str, body: &str) {
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("u.cpp", "#include \"ab.h\"\nusing String = A;\n"),
        (
            "t.cpp",
            "#include \"s.h\"\nint use(String& s) { return s.m(); }\n",
        ),
        (glue, body),
    ]);
}

const GLUE: &str = "#include \"u.cpp\"\n#include \"t.cpp\"\n";

#[test]
fn a_c_plus_plus_extension_glue_file_makes_one_unit_of_the_alias_and_the_caller() {
    glue_unit("all.c++", GLUE);
}

#[test]
fn an_ipp_glue_file_makes_one_unit_of_the_alias_and_the_caller() {
    glue_unit("all.ipp", GLUE);
}

#[test]
fn a_computed_include_makes_the_alias_of_the_included_file_possibly_visible() {
    // `#include UNIT` names no file the graph can read: the caller may include the
    // alias's file, as on `main`, and the site stays open.
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("u.cpp", "#include \"ab.h\"\nusing String = A;\n"),
        (
            "t.cpp",
            "#include \"s.h\"\n#define UNIT \"u.cpp\"\n#include UNIT\nint use(String& s) { return s.m(); }\n",
        ),
    ]);
}

/// A computed include (`#include UNIT`) names no file the graph can read, so it may
/// include any file. The alias stays possibly visible when the file that holds it is
/// included by a file with a computed include, and when a header the caller includes
/// holds one.
#[test]
fn a_computed_include_in_a_file_that_includes_the_alias_makes_it_possibly_visible() {
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("u.cpp", "#include \"ab.h\"\nusing String = A;\n"),
        (
            "t.cpp",
            "#include \"s.h\"\nint use(String& s) { return s.m(); }\n",
        ),
        (
            "all.cpp",
            "#include \"u.cpp\"\n#define UNIT \"t.cpp\"\n#include UNIT\n",
        ),
    ]);
}

#[test]
fn a_computed_include_in_a_header_the_caller_includes_makes_the_alias_possibly_visible() {
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("u.cpp", "#include \"ab.h\"\nusing String = A;\n"),
        ("pick.h", "#define UNIT \"u.cpp\"\n#include UNIT\n"),
        (
            "t.cpp",
            "#include \"s.h\"\n#include \"pick.h\"\nint use(String& s) { return s.m(); }\n",
        ),
    ]);
}

/// The other direction: a file that includes the caller, and that has a computed
/// include or reaches one, may include the file that holds the alias.
#[test]
fn a_computed_include_in_a_file_that_includes_the_caller_makes_the_alias_possibly_visible() {
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("u.cpp", "#include \"ab.h\"\nusing String = A;\n"),
        (
            "t.cpp",
            "#include \"s.h\"\nint use(String& s) { return s.m(); }\n",
        ),
        (
            "glue.cpp",
            "#define UNIT \"u.cpp\"\n#include UNIT\n#include \"t.cpp\"\n",
        ),
    ]);
}

#[test]
fn a_file_that_includes_the_caller_and_a_header_with_a_computed_include_may_see_the_alias() {
    assert_use_stays_open(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("u.cpp", "#include \"ab.h\"\nusing String = A;\n"),
        ("pick.h", "#define UNIT \"u.cpp\"\n#include UNIT\n"),
        (
            "t.cpp",
            "#include \"s.h\"\nint use(String& s) { return s.m(); }\n",
        ),
        ("all.cpp", "#include \"t.cpp\"\n#include \"pick.h\"\n"),
    ]);
}

#[test]
fn an_alias_of_a_c_plus_plus_source_file_no_file_includes_with_the_caller_is_not_seen() {
    let (store, _tmp) = index_and_resolve(&[
        ("s.h", LIB_STRING),
        ("ab.h", A_AND_B),
        ("u.c++", "#include \"ab.h\"\nusing String = A;\n"),
        (
            "t.cpp",
            "#include \"s.h\"\nusing lib::String;\nint use(String& s) { return s.m(); }\n",
        ),
    ]);
    assert_eq!(bound_methods(&store, "use"), ["m -> lib::String::m"]);
}
