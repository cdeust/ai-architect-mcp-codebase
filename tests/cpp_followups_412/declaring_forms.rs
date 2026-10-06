// cpp_followups_412::declaring_forms: a name a scope declares hides the outer ones, whatever
// form declares it. `e.m()` below names a `B` or a name with no type the source writes;
// the outer `e` (a catch parameter, a local) is an `A` and is never the receiver. A form
// the reader of declarations did not know made the walk reach the outer `e` and bind
// `A::m` (issue #412 point 2, rounds 3 to 5). Each form is checked against both outer
// declarations; a name with no readable type leaves the call open as `no_receiver_type`.

use super::*;

const PRELUDE: &str = "\
struct A { int m() const { return 1; } };
struct B { int m() const { return 2; } };
struct P { B b; int c; };
namespace ns { extern B e; }
int g();
";

/// The targets and the unresolved reasons of the `m` sites of `caller`.
fn m_sites(store: &GraphStore, caller: &str) -> (Vec<String>, Vec<String>) {
    let ask = |query: String| -> Vec<String> {
        let mut rows: Vec<String> = store
            .execute_query(&query)
            .expect("query m sites")
            .rows
            .into_iter()
            .map(|r| {
                // The qualified name without its file prefix and its `#n` suffix.
                let name = r[0].split_once("::").map_or(&r[0][..], |(_, q)| q);
                name.rsplit_once('#').map_or(name, |(n, _)| n).to_string()
            })
            .collect();
        rows.sort();
        rows
    };
    let bound = ask(format!(
        "MATCH (cs:CallSite)-[:Calls_CallSite_Method]->(m:Method) \
         WHERE cs.id CONTAINS '{caller}#' AND cs.callee_name = 'm' RETURN m.qualified_name"
    ));
    let reasons = ask(format!(
        "MATCH (cs:CallSite) WHERE cs.id CONTAINS '{caller}#' AND cs.callee_name = 'm' \
         RETURN cs.unresolved_reason"
    ));
    (bound, reasons)
}

/// `body` after an outer declaration of `e` of type `A`: a catch parameter, or a local.
fn sources(name: &str, body: &str) -> [(String, String); 2] {
    [
        (
            format!("c_{name}"),
            format!(
                "{PRELUDE}int c_{name}() {{ try {{ g(); }} catch (const A& e) {{ {body} }} return 0; }}\n"
            ),
        ),
        (
            format!("l_{name}"),
            format!("{PRELUDE}int l_{name}() {{ A e; {body} return 0; }}\n"),
        ),
    ]
}

/// The receiver is declared with no type the source writes: no hint, the call is open.
fn assert_receiver_has_no_type(name: &str, body: &str) {
    for (caller, src) in sources(name, body) {
        let (store, _tmp) = index_and_resolve(&[(&format!("{caller}.cpp"), &src)]);
        assert_eq!(
            m_sites(&store, &caller),
            (vec![], vec!["no_receiver_type".to_string()]),
            "{caller}: {src}"
        );
    }
}

/// The receiver is declared a `B`: it binds `B::m`, never the outer `A::m`.
fn assert_receiver_is_a_b(name: &str, body: &str) {
    for (caller, src) in sources(name, body) {
        let (store, _tmp) = index_and_resolve(&[(&format!("{caller}.cpp"), &src)]);
        assert_eq!(
            m_sites(&store, &caller).0,
            ["B::m".to_string()],
            "{caller}: {src}"
        );
    }
}

macro_rules! forms {
    ($($test:ident: $assert:ident($body:literal))+) => {$(
        #[test]
        fn $test() {
            $assert(stringify!($test), $body);
        }
    )+};
}

forms! {
    a_structured_binding_declares_its_names_without_a_type:
        assert_receiver_has_no_type("{ P q; auto [e, c] = q; return e.m(); }")
    a_structured_binding_by_reference_declares_its_names_without_a_type:
        assert_receiver_has_no_type("{ P q; auto& [e, c] = q; return e.m(); }")
    an_init_capture_declares_its_name_without_a_type:
        assert_receiver_has_no_type("{ auto l = [e = B()]() mutable { return e.m(); }; return l(); }")
    an_init_capture_by_reference_declares_its_name_without_a_type:
        assert_receiver_has_no_type("{ B o; auto l = [&e = o]() { return e.m(); }; return l(); }")
    an_if_init_statement_declares_its_name:
        assert_receiver_is_a_b("if (B e; true) { return e.m(); }")
    a_switch_init_statement_declares_its_name:
        assert_receiver_is_a_b("switch (B e; 1) { default: return e.m(); }")
    a_declaration_under_a_label_declares_its_name:
        assert_receiver_is_a_b("{ L: B e; return e.m(); }")
    a_declaration_under_a_case_declares_its_name:
        assert_receiver_is_a_b("switch (1) { case 1: B e; break; default: return e.m(); }")
    a_range_for_structured_binding_declares_its_names_without_a_type:
        assert_receiver_has_no_type("{ P arr[1]; for (auto [e, c] : arr) { return e.m(); } }")
    a_range_for_init_statement_declares_its_name:
        assert_receiver_is_a_b("{ int arr[1]; for (B e; int x : arr) { return e.m(); } }")
    a_using_declaration_declares_its_name_without_a_type:
        assert_receiver_has_no_type("{ using ns::e; return e.m(); }")
    a_declaration_under_a_preprocessor_condition_declares_its_name_without_a_type:
        assert_receiver_has_no_type("{\n#ifdef X\n B e;\n#endif\n return e.m(); }")
    a_variadic_lambda_parameter_declares_its_name_without_a_type:
        assert_receiver_has_no_type("{ auto l = [](auto&... e) { return (e.m() + ...); }; return 0; }")
    a_lambda_template_parameter_declares_its_name:
        assert_receiver_is_a_b("{ auto l = []<B e>() { return e.m(); }; return 0; }")
    a_requires_parameter_declares_its_name:
        assert_receiver_is_a_b("{ bool ok = requires (B e) { e.m(); }; return ok; }")
    an_attributed_declarator_declares_its_name:
        assert_receiver_is_a_b("{ B e [[maybe_unused]] = B(); return e.m(); }")
    a_parenthesised_declarator_declares_its_name:
        assert_receiver_is_a_b("{ const B (e) = B(); return e.m(); }")
    a_declaration_with_a_brace_initialiser_declares_its_name:
        assert_receiver_is_a_b("{ B e{}; return e.m(); }")
    a_declaration_with_a_constructor_call_declares_its_name:
        assert_receiver_is_a_b("{ B e(1); return e.m(); }")
    a_direct_initialisation_from_a_name_declares_its_name:
        assert_receiver_is_a_b("{ int y = 0; B e(y); return e.m(); }")
    a_direct_initialisation_from_a_call_declares_its_name:
        assert_receiver_is_a_b("{ B e(g()); return e.m(); }")
    a_static_declaration_declares_its_name:
        assert_receiver_is_a_b("{ static B e; return e.m(); }")
    an_inner_block_declares_its_name:
        assert_receiver_is_a_b("{ B e; return e.m(); }")
    a_for_init_declares_its_name:
        assert_receiver_is_a_b("for (B e;;) { return e.m(); }")
    an_if_condition_declaration_declares_its_name:
        assert_receiver_is_a_b("if (B* e = new B) { return e->m(); }")
    a_lambda_parameter_declares_its_name:
        assert_receiver_is_a_b("{ auto l = [](B& e) { return e.m(); }; return 0; }")
}

/// A name no scope between the handler and the call declares again is the catch
/// parameter or the local: the reader does not drop what it can read.
#[test]
fn an_outer_name_a_nested_scope_does_not_redeclare_keeps_its_type() {
    for (caller, src) in sources("outer", "{ B other; (void)other; return e.m(); }") {
        let (store, _tmp) = index_and_resolve(&[(&format!("{caller}.cpp"), &src)]);
        assert_eq!(m_sites(&store, &caller).0, ["A::m".to_string()], "{caller}");
    }
}

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
    assert_catch_param_is_not_the_receiver("{ DECLARE_VAR B e; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ __extension__ B e; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ e e e; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ B e @@; return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ [[attr]] DECLARE(e); return e.m(); }");
}

#[test]
fn a_statement_that_does_not_name_the_catch_parameter_keeps_its_type() {
    let src = format!(
        "{PRELUDE}int c() {{ try {{ g(); }} catch (const A& e) {{ LOG(1, 2); return e.m(); }} return 0; }}\n"
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
}

/// The price of reading a call for a declaration, said in a test: any call that takes
/// the catch parameter or a macro name stands for a declaration, so the type is not read
/// past it: the site stays open, as on `main`.
#[test]
fn a_call_that_takes_the_catch_parameter_or_a_macro_name_declines_the_type() {
    assert_catch_param_is_not_the_receiver("{ handle(e); return e.m(); }");
    assert_catch_param_is_not_the_receiver("{ log(e.what()); return e.m(); }");
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
        "{ TABLE[2] = 1; return e.m(); }",
        "{ p.c = 1; return e.m(); }",
    ] {
        let src = format!(
            "{PRELUDE}int TABLE[3]; P p;\nint c() {{ try {{ g(); }} catch (const A& e) {{ {body} }} return 0; }}\n"
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
