// cpp_followups_412::catch_guard: the type of a catch parameter, which `main` never read, is
// used only when nothing between the handler and the call could declare the name in a form the
// reader does not know. A form that may declare it leaves the call open; a form that cannot
// keeps `A::m`. source: ADR-9847.

use super::declaring_forms::{m_sites, PRELUDE};
use super::*;

/// A catch parameter is typed only when nothing between the handler and the call could
/// declare the name in a form the reader does not know: `main` never typed it, so a
/// receiver typed from it there would bind where `main` stays open. A local `A e;` is
/// read as on `main` and is not guarded.
fn assert_catch_param_is_not_the_receiver(body: &str) {
    let src = format!(
        "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ {body} }} return 0; }}\n"
    );
    let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
    let (bound, sites) = m_sites(&store, "c");
    // The call is a site (a test that finds none passes for the wrong reason).
    assert!(!sites.is_empty(), "no `m` site: {src}");
    assert!(!bound.contains(&"A::m".to_string()), "{bound:?}: {src}");
}

/// The call reads the catch parameter, whose type nothing before it could change.
fn assert_catch_param_keeps_its_type(body: &str) {
    let src = format!(
        "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ {body} }} return 0; }}\n"
    );
    let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
    assert_eq!(m_sites(&store, "c").0, ["A::m".to_string()], "{src}");
}

/// The statements of these bodies are a parse error that costs the call its site, as
/// the CHANGELOG says: nothing is bound, whatever the catch parameter is.
fn assert_call_is_not_extracted(body: &str) {
    let src = format!(
        "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ {body} }} return 0; }}\n"
    );
    let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
    let (bound, sites) = m_sites(&store, "c");
    assert!(
        sites.is_empty() && bound.is_empty(),
        "{bound:?} {sites:?}: {src}"
    );
}

#[test]
fn a_macro_statement_naming_the_catch_parameter_may_declare_it() {
    assert_catch_param_is_not_the_receiver("{ DECLARE_VAR(B, e); return e.m(); }");
    assert_catch_param_is_not_the_receiver("DECLARE_VAR(B, e); return e.m();");
    assert_catch_param_is_not_the_receiver("{ L: DECLARE_VAR(B, e); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B (e); return e.m(); }");
}

#[test]
fn a_statement_of_an_unknown_kind_naming_the_catch_parameter_may_declare_it() {
    assert_catch_param_is_not_the_receiver("{ [[attr]] DECLARE(e); return e.m(); }");
}

/// What the reader reads itself, with a macro or an extension in front: the name is declared
/// as a `B`, and `B::m` is the right target. The guard is not what keeps these off `A::m`.
#[test]
fn a_declaration_the_reader_reads_types_the_name_it_declares() {
    for body in [
        "{ DECLARE_VAR B e; return e.m(); }",
        "{ __extension__ B e; return e.m(); }",
        "{ B e @@; return e.m(); }",
    ] {
        let src = format!(
            "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ {body} }} return 0; }}\n"
        );
        let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
        assert_eq!(m_sites(&store, "c").0, ["B::m".to_string()], "{src}");
    }
}

#[test]
fn a_statement_that_does_not_name_the_catch_parameter_keeps_its_type() {
    let src = format!(
        "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ log(1, 2); return e.m(); }} return 0; }}\n"
    );
    let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
    assert_eq!(m_sites(&store, "c").0, ["A::m".to_string()]);
}

/// A label, a `case` and a preprocessor branch hold statements of their own: one that
/// names the catch parameter without declaring it leaves the type in place.
#[test]
fn a_labelled_or_conditional_statement_that_cannot_declare_the_name_keeps_its_type() {
    for body in [
        "{ L: log(e.v); return e.m(); }",
        "switch (1) { case 1: log(e.v); break; default: return e.m(); }",
        "{\n#ifdef X\n log(e.v);\n#endif\n return e.m(); }",
    ] {
        let src = format!(
            "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ {body} }} return 0; }}\n"
        );
        let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
        assert_eq!(m_sites(&store, "c").0, ["A::m".to_string()], "{src}");
    }
}

/// A macro statement that declares the name in a shape the reader cannot see: the
/// argument is not the bare name (`VAR(B* e);`), the call has no argument at all
/// (`DECLARE_ALL();`), or the declaration's declarator is a macro name.
#[test]
fn a_macro_statement_with_a_declaration_argument_may_declare_the_catch_parameter() {
    assert_catch_param_is_not_the_receiver("{ VAR(B* e); return e->m(); }");
    assert_catch_param_is_not_the_receiver("{ VAR(B& e = b); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ VAR(B, e = B()); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ VAR2(B, *e); return e->m(); }");
    assert_catch_param_is_not_the_receiver("{ VAR3(B * e); return e->m(); }");
    assert_catch_param_is_not_the_receiver("{ do { VAR(B* e); return e->m(); } while (0); }");
}

#[test]
fn a_macro_statement_without_arguments_may_declare_the_catch_parameter() {
    assert_catch_param_is_not_the_receiver("{ DECLARE_ALL(); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ DECL_E if (1) { return e.m(); } }");
    assert_catch_param_is_not_the_receiver("{ DECL_E; return e.m(); }");
}

/// A macro before a statement keyword is a parse error, and the parse error costs the
/// call its site (the CHANGELOG says so): nothing is bound, nothing is open. `# B e;`
/// is a stray directive that does the same.
#[test]
fn a_statement_the_grammar_cannot_read_at_all_costs_the_call_its_site() {
    assert_call_is_not_extracted("{ DECL_E return e.m(); }");
    assert_call_is_not_extracted("{ L1: DECL_E return e.m(); }");
    assert_call_is_not_extracted("{ # B e; return e.m(); }");
}

/// A parse error beside the name hides what the statement declares: `B x e;` may be
/// `B x EXPORT_e;` once the preprocessor has run, and what no grammar reads (`# B e;`,
/// `x y z e;`, `@ B e 1;`, `DECL_E )`) is left to the compiler.
#[test]
fn a_parse_error_beside_the_catch_parameter_may_hide_its_declaration() {
    assert_catch_param_is_not_the_receiver("{ B x e; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B x(e; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ x y z e; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ DECL_E ) return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ @ B e 1; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ ) B e :: ; return e.m(); }");
}

#[test]
fn a_declaration_with_a_macro_declarator_may_declare_the_catch_parameter() {
    assert_catch_param_is_not_the_receiver("{ B NAMED(e); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B DECL_E; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B e BRACES; return e.m(); }");
}

/// The macro name is the declarator under an initialiser, a pointer, a reference or an
/// array: `#define DECL_E e` makes `B DECL_E = B();` a declaration of `e`.
#[test]
fn a_macro_declarator_under_a_wrapping_declarator_may_declare_the_catch_parameter() {
    assert_catch_param_is_not_the_receiver("{ B DECL_E = B(); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B NAMED(e) = B(); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B* NAMED(e); return e->m(); }");
    assert_catch_param_is_not_the_receiver("{ B* DECL_E = nullptr; return e->m(); }");
    assert_catch_param_is_not_the_receiver("{ B& DECL_E = b; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B DECL_E[2]; return e->m(); }");
}

/// No list of wrapping kinds: an attribute, a second declarator, a structured binding, a
/// qualifier all wrap the macro name and none hides it.
#[test]
fn a_macro_name_anywhere_in_a_declarator_may_declare_the_catch_parameter() {
    for body in [
        "{ B DECL_E [[maybe_unused]]; return e.m(); }",
        "{ B DECL_E [[maybe_unused]] = B(); return e.m(); }",
        "{ B [[maybe_unused]] DECL_E; return e.m(); }",
        "{ B DECL_E, y; return e.m(); }",
        "{ B y, DECL_E; return e.m(); }",
        "{ B y = B(), DECL_E = B(); return e.m(); }",
        "{ B *const DECL_E = 0; return e->m(); }",
        "{ auto [DECL_E, z] = pr; return e.m(); }",
        "{ auto& DECL_E = b; return e.m(); }",
    ] {
        assert_catch_param_is_not_the_receiver(body);
    }
}

/// A macro call that starts an initialisation or an array: `#define DECLARE(T, n) T n`
/// makes `DECLARE(B, e) = B();` a declaration of `e`. A call of a call
/// (`DECLARE(B, e)(B());`) and a call the grammar reads for a declaration whose only
/// macro is an argument (`B (&DECL_E) = b;`) are read as well.
#[test]
fn a_macro_call_that_starts_an_initialisation_may_declare_the_catch_parameter() {
    assert_catch_param_is_not_the_receiver("{ DECLARE(B, e) = B(); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ DECLARE(B, e)[2]; return e->m(); }");
    assert_catch_param_is_not_the_receiver("{ *DECLARE(B, e) = nullptr; return e->m(); }");
    assert_catch_param_is_not_the_receiver("{ DECLARE(B, e)(B()); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B (DECL_E); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B (&DECL_E) = b; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B (*DECL_E); return e->m(); }");
    assert_catch_param_is_not_the_receiver("{ B ((DECL_E)); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B (&(DECL_E)) = b; return e.m(); }");
}

/// The price of reading a call for a declaration, said in a test: any call that takes
/// the catch parameter or a macro name stands for a declaration, so the type is not read
/// past it: the site stays open, as on `main`.
#[test]
fn a_call_that_takes_the_catch_parameter_or_a_macro_name_declines_the_type() {
    assert_catch_param_is_not_the_receiver("{ handle(e); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ LOG(1, 2); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ TABLE[2] = 1; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ close(FD); return e.m(); }");
}

/// The header of a statement that encloses the call is read as a statement before it:
/// a macro that declares there (`for (B DECL_E : ys)`, `if (B DECL_E = B(); 1)`,
/// `while (B DECL_E = B())`, `for (DECL_E;;)`) names the variable the call reads.
#[test]
fn a_macro_in_the_header_of_a_statement_that_encloses_the_call_may_declare_the_catch_parameter() {
    for body in [
        "for (B DECL_E; ;) { return e->m(); }",
        "for (B DECL_E = B(); ;) { return e->m(); }",
        "for (B DECL_E : ys) { return e->m(); }",
        "for (B* DECL_E : ys) { return e->m(); }",
        "if (B DECL_E = B(); 1) { return e->m(); }",
        "while (B DECL_E = B()) { return e->m(); }",
        "switch (B DECL_E = B(); 1) { case 1: return e->m(); }",
        "for (DECL_E; ;) { return e->m(); }",
        "if (DECL_E; 1) { return e->m(); }",
        "for (DECLARE(B, e); ;) { return e->m(); }",
    ] {
        assert_catch_param_is_not_the_receiver(&format!("{{ {body} }}"));
    }
}

/// A macro name in the initial value, the size of an array or the parameters of a
/// function declarator names nothing: the type is kept.
#[test]
fn a_macro_name_in_a_value_a_size_or_a_parameter_keeps_the_type() {
    for stmt in ["int n = MAX_LEN;", "int a[SIZE_N];", "int h(int PARAM);"] {
        let src = format!(
            "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ {stmt} return e.m(); }} return 0; }}\n"
        );
        let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
        assert_eq!(m_sites(&store, "c").0, ["A::m".to_string()], "{src}");
    }
}

/// What the guard must not swallow: a macro that declares in a scope that is closed
/// before the call, or a type (`using`, `typedef`), names no variable the call reads.
#[test]
fn a_macro_declaration_in_a_scope_closed_before_the_call_keeps_the_type() {
    for stmt in [
        "if (1) { B DECL_E; }",
        "for (B DECL_E; ;) { break; }",
        "for (B DECL_E : ys) { }",
        "while (B DECL_E = B()) { break; }",
        "typedef B DECL_E;",
        "using DECL_E = B;",
    ] {
        let src = format!(
            "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ {stmt} return e.m(); }} return 0; }}\n"
        );
        let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
        assert_eq!(m_sites(&store, "c").0, ["A::m".to_string()], "{src}");
    }
}

/// What the unwrapping must not swallow: an assignment to a plain name or a member, an
/// element of a constant array, a lower case initialised local.
#[test]
fn an_assignment_that_is_not_a_macro_declaration_keeps_the_type_of_the_catch_parameter() {
    for body in [
        "{ B other = B(); (void)other; return e.m(); }",
        "{ int n = 0; n = 1; return e.m(); }",
        "{ table[2] = 1; return e.m(); }",
        "{ p.c = 1; return e.m(); }",
    ] {
        let src = format!(
            "{PRELUDE}int table[3]; P p;\nint c() {{ try {{ g(); }} catch (const A& e) {{ {body} }} return 0; }}\n"
        );
        let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
        assert_eq!(m_sites(&store, "c").0, ["A::m".to_string()], "{src}");
    }
}

/// What the macro rule must not swallow: a call whose arguments only read the name.
#[test]
fn a_call_that_only_reads_a_member_of_the_catch_parameter_keeps_its_type() {
    for body in [
        "{ log(e.v); return e.m(); }",
        "{ log(e.v, g()); return e.m(); }",
        "{ g(); return e.m(); }",
    ] {
        let src = format!(
            "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ {body} }} return 0; }}\n"
        );
        let (store, _tmp) = index_and_resolve(&[("c.cpp", &src)]);
        assert_eq!(m_sites(&store, "c").0, ["A::m".to_string()], "{src}");
    }
}

/// Where a declaration hides that is not a `declaration`: the parameters of a lambda, of a
/// nested handler or of a `requires` expression, a capture, a `using`, a comma expression,
/// the initialiser of a `for`, an attribute in front of a statement. Two reviews of #438
/// listed these; each was run against the indexer.
#[test]
fn a_macro_or_the_name_where_no_declaration_is_may_declare_the_catch_parameter() {
    for body in [
        "{ auto l = [DECL_E = B()]() { return e.m(); }; return l(); }",
        "{ B o; auto l = [&DECL_E = o]() { return e.m(); }; return l(); }",
        "{ auto l = [](B DECL_E) { return e.m(); }; return l(B()); }",
        "{ auto l = [](B* DECL_E) { return e->m(); }; return l(nullptr); }",
        "{ auto l = [](auto DECL_E) { return e.m(); }; return l(B()); }",
        "{ auto l = [](B DECL_E = B()) { return e.m(); }; return l(); }",
        "{ auto l = []<B DECL_E>() { return e.m(); }; return 0; }",
        "{ try { g(); } catch (B DECL_E) { return e.m(); } }",
        "{ try { g(); } catch (const B& DECL_E) { return e.m(); } }",
        "{ bool ok = requires (B DECL_E) { e.m(); }; return ok; }",
        "{ using ns::DECL_E; return e.m(); }",
        "{ DECLARE(B, e), g(); return e.m(); }",
        "{ for (DECLARE(B, DECL_E); ;) { return e.m(); } }",
        "{ for (DECLARE_ALL(); ;) { return e.m(); } }",
        "{ [[maybe_unused]] DECL_BE; return e.m(); }",
        "{ [[maybe_unused]] DECLARE(B, DECL_E); return e.m(); }",
        "{ DECL_E(1); return e.m(); }",
        "{ DECLARE_E(B); return e.m(); }",
        "{ [[maybe_unused]] DECL_E; return e.m(); }",
        "{ DECL_E: return e.m(); }",
        "{ for (DECL_E(1); ;) { return e.m(); } }",
        "{ if (DECL_E(1); true) { return e.m(); } }",
        "{ B (DECL_E) = B(); return e.m(); }",
        "{ DECL_E[2]; return e->m(); }",
        "{ DECL_E = B(); return e.m(); }",
        "{ DECL_T DECL_E(1); return e.m(); }",
        "{ DECL_E LOG(1); return e.m(); }",
        "{ DWORD n; return e.m(); }",
        "{\n#include \"decl_e.inc\"\n return e.m(); }",
        "{\n#define DECL_E B e\n return e.m(); }",
    ] {
        assert_catch_param_is_not_the_receiver(body);
    }
}

/// What the guard keeps typed: a declaration in a scope closed before the call, a `using`
/// of a function, a reading of the catch parameter (`e.v`, `e.ok()`).
#[test]
fn a_statement_that_declares_nothing_the_call_reads_keeps_the_type() {
    for body in [
        "{ auto l = [](B DECL_E) { return 0; }; return e.m(); }",
        "{ using ns::f; return e.m(); }",
        "{ for (DECLARE_X(); ;) { break; } return e.m(); }",
        "{ return e.v + e.m(); }",
        "{ if (e.ok()) { return e.m(); } return 0; }",
        "{ return e.ok() && e.m(); }",
        "{\n#ifdef DEBUG_MODE\n log(1);\n return e.m();\n#endif\n }",
        "{\n#if DEBUG_LEVEL > 1\n return e.m();\n#endif\n }",
        "{\n#if 0\n#elif DEBUG_LEVEL > 1\n return e.m();\n#endif\n }",
        "{\n#ifdef OTHER_MODE\n#elifdef DEBUG_MODE\n return e.m();\n#endif\n }",
        "{ return e.m(); LATE_MACRO; }",
        "{ decltype(e) x = e; return e.m(); }",
        "{ return DECL_E; return e.m(); }",
        "{ throw DECL_E; return e.m(); }",
        "{ goto DECL_L; return e.m(); }",
        "{ co_return DECL_E; return e.m(); }",
        "{ co_yield DECL_E; return e.m(); }",
        "{ g(e[0]); return e.m(); }",
        "{ return LOG_IT(e.m()); }",
        "{ { B DECL_E; } return e.m(); }",
        "{ if (1) { B DECL_E; } return e.m(); }",
        "{ while (1) { B DECL_E; break; } return e.m(); }",
        "{ do { B DECL_E; } while (0); return e.m(); }",
        "{ for (;;) { B DECL_E; break; } return e.m(); }",
        "{ for (B x : ys) { B DECL_E; } return e.m(); }",
        "{ switch (1) { case 1: B DECL_E; } return e.m(); }",
        "{ try { B DECL_E; } catch (...) { } return e.m(); }",
        "{ typedef B DECL_E; return e.m(); }",
        "{ using DECL_E = B; return e.m(); }",
        "{ static_assert(MAX_N > 1); return e.m(); }",
        "{ namespace NS_ALIAS = ns; return e.m(); }",
        "{ A e; if (1) { DECL_E; return e.m(); } }",
        "{ A e; { DECL_E; return e.m(); } }",
        "{ A e; for (;;) { DECL_E; return e.m(); } }",
    ] {
        assert_catch_param_keeps_its_type(body);
    }
}

/// Where the preprocessor or the mask hides a declaration: a directive under `#if`, a token
/// the mask erased (a macro in front of a type), a macro written as a member name.
#[test]
fn a_directive_an_erased_token_or_a_macro_member_may_declare_the_catch_parameter() {
    for body in [
        "{\n#if 1\n#define mk B e\n#endif\nmk; return e.m(); }",
        "{\n#ifdef cfg\n#include \"decl_e.inc\"\n#endif\n return e.m(); }",
        "{\n#ifdef cfg\n#define decl_e B e\n#endif\n return e.m(); }",
        "{\n#define mk B e\nmk; return e.m(); }",
        "{ DECL_E int s; return e.m(); }",
        "{ DECL_E B x; return e.m(); }",
        "{ g(); DECL_E B y; return e.m(); }",
        "{ DECL_E B* px; return e.m(); }",
        "{ static DECL_E B x; return e.m(); }",
        "{ L: DECL_E B x; return e.m(); }",
        "{ int f(int) DECL_E; return e.m(); }",
        "{\n#if X\n B x\n#else\n B DECL_E\n#endif\n ; return e.m(); }",
        "{ obj.FIELD_DECL_E; return e.m(); }",
        "{ p->FIELD_DECL_E; return e.m(); }",
        "{\n#define mk(x) B e\nmk(1); return e.m(); }",
        "{\n#undef DECL_E\n return e.m(); }",
        "{\n#pragma once\n return e.m(); }",
    ] {
        assert_catch_param_is_not_the_receiver(body);
    }
}
