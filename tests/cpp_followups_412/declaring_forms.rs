// cpp_followups_412::declaring_forms: a name a scope declares hides the outer ones, whatever
// form declares it. `e.m()` below names a `B` or a name with no type the source writes;
// the outer `e` (a catch parameter, a local) is an `A` and is never the receiver. A form
// the reader of declarations did not know made the walk reach the outer `e` and bind
// `A::m` (issue #412 point 2, rounds 3 to 5). Each form is checked against both outer
// declarations; a name with no readable type leaves the call open as `no_receiver_type`.

use super::*;

pub(super) const PRELUDE: &str = "\
struct A { int m() const { return 1; } };
struct B { int m() const { return 2; } };
struct P { B b; int c; };
namespace ns { extern B e; }
int g();
";

/// The targets and the unresolved reasons of the `m` sites of `caller`.
pub(super) fn m_sites(store: &GraphStore, caller: &str) -> (Vec<String>, Vec<String>) {
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
