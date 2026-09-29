// Tests for parser::header_dialect (#399).

use super::*;

const TEMPLATE_HEADER: &str = "#pragma once\n\
namespace etl {\n\
template <typename T, int N>\n\
class vector {\n\
public:\n\
    void push_back(const T& v);\n\
};\n\
}\n";

const GUARDED_C_HEADER: &str = "#ifndef TASK_H\n#define TASK_H\n\
#ifdef __cplusplus\n\
extern \"C\" {\n\
#endif\n\
void *pvPortMalloc(unsigned long xSize);\n\
typedef struct tskTaskControlBlock * TaskHandle_t;\n\
#ifdef __cplusplus\n\
}\n\
#endif\n\
#endif\n";

fn auto(source: &str) -> Language {
    header_language(None, source)
}

#[test]
fn a_templated_class_header_is_cpp() {
    assert_eq!(auto(TEMPLATE_HEADER), Language::Cpp);
}

#[test]
fn a_c_header_guarded_for_cplusplus_stays_c() {
    assert_eq!(auto(GUARDED_C_HEADER), Language::C);
}

#[test]
fn each_cpp_construct_alone_is_enough() {
    for src in [
        "namespace a { int f(void); }",
        "namespace { int x; }",
        "using namespace std;",
        "template <class T> T id(T v);",
        "class Queue { int n; };",
        "class Derived : Base { };",
        "class Leaf final { };",
        "struct S { public: int x; };",
        "int Owner::count(void);",
        "void f() { ::g(); }",
    ] {
        assert_eq!(auto(src), Language::Cpp, "{src}");
    }
}

#[test]
fn cpp_words_in_comments_literals_and_attributes_stay_c() {
    let src = "/* a class template <T> in namespace x */\n\
               // std::vector, public:\n\
               static const char *s = \"a::b namespace c {\";\n\
               static const char c = ':';\n\
               [[gnu::unused]] static int x;\n\
               int class_count(void);\n\
               int class;\n";
    assert_eq!(auto(src), Language::C);
}

#[test]
fn cpp_only_sections_under_a_cplusplus_test_do_not_move_a_c_header() {
    let src = "int c_api(void);\n\
               #if defined(__cplusplus)\n\
               namespace wrap { class W { }; }\n\
               #else\n\
               int also_ignored(void);\n\
               #endif\n\
               int after(void);\n";
    assert_eq!(auto(src), Language::C);
}

#[test]
fn a_construct_after_the_cplusplus_group_closes_still_counts() {
    let src = "#ifdef __cplusplus\nextern \"C\" {\n#endif\n\
               #ifdef __cplusplus\n}\n#endif\n\
               template <typename T> struct Box { T v; };\n";
    assert_eq!(auto(src), Language::Cpp);
}

#[test]
fn a_continued_directive_hides_its_whole_line() {
    let src = "#define SCOPE(x) \\\n    ns::x\nint f(void);\n";
    assert_eq!(auto(src), Language::C);
}

#[test]
fn a_language_filter_decides_for_every_header() {
    assert_eq!(
        header_language(Some(Language::Cpp), GUARDED_C_HEADER),
        Language::Cpp
    );
    assert_eq!(
        header_language(Some(Language::ObjC), GUARDED_C_HEADER),
        Language::ObjC
    );
    assert_eq!(
        header_language(Some(Language::C), TEMPLATE_HEADER),
        Language::C
    );
}

#[test]
fn only_c_family_filters_keep_headers() {
    assert!(filter_keeps_headers(Language::C));
    assert!(filter_keeps_headers(Language::Cpp));
    assert!(filter_keeps_headers(Language::ObjC));
    assert!(!filter_keeps_headers(Language::Rust));
    assert!(is_shared_header("h"));
    assert!(!is_shared_header("hpp"));
}

/// The module doc's reason for not deciding by error count: the C grammar
/// reports `extern "C" {` as an error, so a clean C header would read as
/// "not C".
#[test]
fn a_c_header_with_a_cplusplus_guard_is_not_error_free_under_c() {
    let parsed = crate::parser::parse_file(GUARDED_C_HEADER, "task.h", Language::C).unwrap();
    assert!(
        parsed.parse_errors > 0,
        "tree-sitter-c accepted extern \"C\" {{"
    );
}
