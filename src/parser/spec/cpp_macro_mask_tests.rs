use super::mask_specifier_macros;

// source: tree-sitter-cpp 0.23.4 on ETLCPP 7d604f2e `string.h`: each shape
// below (trailing `ETL_NOEXCEPT` / `ETL_OVERRIDE`, leading `ETL_CONSTANT` /
// `ETL_EXPLICIT_STRING_FROM_CHAR`) produced an ERROR node.
fn masked(src: &str) -> String {
    mask_specifier_macros(src).unwrap_or_else(|| src.to_string())
}

#[test]
fn blanks_trailing_specifiers_after_a_parameter_list() {
    let src = "void f(int a) ETL_NOEXCEPT ETL_OVERRIDE { }";
    let m = masked(src);
    assert_eq!(m.len(), src.len());
    assert!(!m.contains("ETL_"));
    assert!(m.starts_with("void f(int a)"));
    assert!(m.ends_with("{ }"));
}

#[test]
fn blanks_leading_specifiers_before_a_declaration() {
    let m =
        masked("struct S { ETL_CONSTANT size_t N = 3; ETL_EXPLICIT_STRING_FROM_CHAR S(int x); };");
    assert!(!m.contains("ETL_"));
    assert!(m.contains("size_t N = 3;"));
    assert!(m.contains("S(int x);"));
}

#[test]
fn keeps_function_like_macros_and_plain_words() {
    for src in [
        "void f() { ETL_ASSERT(x, ETL_ERROR(e)); }",
        "int x = LIMIT_MAX;",
        "void f() { return SOME_VALUE; }",
        "HANDLE h;",
        "SECURITY_ATTRIBUTES sa;",
        "// ETL_NOEXCEPT in a comment\nint a;",
        "const char* s = \"ETL_NOEXCEPT\";",
        "#define ETL_NOEXCEPT noexcept\nint a;",
    ] {
        assert_eq!(mask_specifier_macros(src), None, "{src}");
    }
}

#[test]
fn keeps_offsets_and_lines() {
    let src = "class A {\n  void f() ETL_NOEXCEPT;\n};\n";
    let m = masked(src);
    assert_eq!(m.len(), src.len());
    assert_eq!(m.lines().count(), src.lines().count());
}

#[test]
fn blanks_a_macro_after_a_specifier_keyword_and_after_if() {
    let m = masked("struct S { static ETL_CONSTANT size_t N = 3; };");
    assert!(!m.contains("ETL_CONSTANT") && m.contains("static"));
    let m = masked("void f() { if ETL_IF_CONSTEXPR (x) { g(); } }");
    assert!(!m.contains("ETL_IF_CONSTEXPR") && m.contains("if "));
}

#[test]
fn blanks_a_macro_call_at_declaration_level_only() {
    use super::blank_scope_macro_calls as blank;
    let class =
        "class A : public B {\npublic:\n  ETL_STATIC_ASSERT((N > 0U), \"zero\");\n  int x;\n};";
    let out = blank(class).expect("class-level call is blanked");
    assert!(!out.contains("ETL_STATIC_ASSERT") && out.contains("int x;"));
    assert_eq!(out.len(), class.len());
    // In a function body the arguments hold real calls: never touched.
    assert_eq!(blank("void f() { ETL_ASSERT(g(), \"m\"); }"), None);
}

#[test]
fn blanks_a_macro_statement_that_needs_no_semicolon() {
    use super::blank_scope_macro_calls as blank;
    // source: ETLCPP 7d604f2e `enum_type.h`: the three macros open and close
    // braces the parser cannot see.
    let src = "struct S {\n  enum e { A };\n  ETL_DECLARE_ENUM_TYPE(S, int)\n  ETL_ENUM_TYPE(A, \"a\")\n  ETL_END_ENUM_TYPE\n};";
    let out = blank(src).expect("the idiom is blanked");
    assert_eq!(out.len(), src.len());
    assert!(!out.contains("ETL_"), "{out}");
    assert!(out.contains("enum e { A };") && out.trim_end().ends_with("};"));
    // Not a macro statement: a call before a declaration, a bare name before
    // one, and anything inside a function body.
    assert_eq!(blank("struct S { A_B(x) int y; };"), None);
    assert_eq!(blank("struct S { A_B int y; };"), None);
    assert_eq!(blank("void f() { A_B(1) C_D }"), None);
}

#[test]
fn collapses_conditionals_to_their_first_branch() {
    use super::collapse_conditionals as collapse;
    let src = "#if A\nvirtual void r() X\n#else\nvoid r()\n#endif\n{ }\n";
    let out = collapse(src).expect("conditional collapsed");
    assert_eq!(out.len(), src.len());
    assert_eq!(out.lines().count(), src.lines().count());
    assert!(out.contains("virtual void r() X"));
    assert!(!out.contains("void r()\n") && !out.contains("#"));
    assert!(out.contains("{ }"));
    assert_eq!(collapse("int a;\n"), None);
    // Whole declarations in each branch: the `#else` holds definitions.
    assert_eq!(
        collapse("#if A\nstruct X {};\n#else\nstruct Y {};\n#endif\n"),
        None
    );
}

#[test]
fn an_unclosed_call_is_left_alone() {
    let src = "class A {\n  A_B( ;\n  A_C(x);\n};\n";
    let out = super::blank_scope_macro_calls(src).expect("the closed call is blanked");
    assert!(out.contains("A_B( ;"), "{out}");
    assert!(!out.contains("A_C"), "{out}");
    assert!(super::blank_scope_macro_calls("A_B( ;\nA_C( ;\n").is_none());
}

#[test]
fn parens_match_in_one_pass() {
    let toks = super::lex("f(g(1), (2)) ) (");
    let closes = super::paren_closes(&toks);
    let at = |n: usize| {
        toks.iter()
            .enumerate()
            .filter(|(_, t)| t.tok == super::Tok::Punct(b'('))
            .nth(n)
            .map(|(i, _)| closes[i])
    };
    assert_eq!(at(0), Some(Some(10)));
    assert_eq!(at(3), Some(None));
}
