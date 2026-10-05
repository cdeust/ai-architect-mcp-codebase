// parser::spec::cpp_receiver — what the parser can say about the receiver of a
// C++ call (issue #406).
//
// `member_access_callee` reduces every C++ callee to its last identifier, so
// `pb->empty()`, `ru.empty()` and `etl::width(3)` reach the resolver as the
// bare names `empty` and `width`. This module keeps the two facts the source
// states about the receiver, as a `receiver_hint` and a `receiver_hint_via`:
//
// - `cpp-this`: the receiver is `this`, `*this` or `(*this)`: the caller's own
//   class.
// - `cpp-declared`: the receiver is a name that a parameter, a local, a range
//   variable, a condition or a catch clause (`catch (const E& e)`, in its handler
//   only) declares with a type written in the source (`Bloom* p`); the hint is that
//   type without qualifier, pointer, reference or generic arguments. A name declared
//   with `auto`, a structured binding, an init-capture or a `using` (no type written)
//   gives no hint and hides the outer declarations of that name (`cpp_declared`); so
//   does a name no enclosing scope declares (a field, a global), a type that is a
//   template parameter of an enclosing template or any other expression.
// - `cpp-qualifier`: the callee is written `a::b::f`; the hint is `a::b`.
//
// source: tree-sitter-cpp 0.23.4 node-types.json (field_expression.argument,
// declaration.{type,declarator}, parameter_declaration, for_range_loop,
// condition_clause.value, qualified_identifier.scope).

use tree_sitter::Node;

use super::cpp_declared::{declared_type, plain_type_text};
use crate::graph_store::{
    RECEIVER_HINT_VIA_CPP_DECLARED, RECEIVER_HINT_VIA_CPP_QUALIFIER, RECEIVER_HINT_VIA_CPP_THIS,
};
use crate::parser::node_text;

/// The `receiver_hint` and `receiver_hint_via` properties of one call, or none
/// when the source states nothing about its receiver.
pub(super) fn receiver_props(
    source: &str,
    call_node: Node,
    function_field: &str,
) -> Vec<(String, String)> {
    let Some(callee) = call_node.child_by_field_name(function_field) else {
        return Vec::new();
    };
    let read = match callee.kind() {
        "field_expression" => member_receiver(source, callee),
        "qualified_identifier" | "template_function" => qualifier(source, callee),
        _ => None,
    };
    match read {
        Some((hint, via)) => vec![
            ("receiver_hint".to_string(), hint),
            ("receiver_hint_via".to_string(), via.to_string()),
        ],
        None => Vec::new(),
    }
}

/// `a::b` of a callee written `a::b::f` (generic arguments dropped); empty for
/// `::f`. Not qualified: `None`.
fn qualifier(source: &str, callee: Node) -> Option<(String, &'static str)> {
    let text = plain_type_text(&node_text(source, callee));
    let (scope, _) = text.rsplit_once("::")?;
    Some((scope.to_string(), RECEIVER_HINT_VIA_CPP_QUALIFIER))
}

fn member_receiver(source: &str, callee: Node) -> Option<(String, &'static str)> {
    let mut argument = callee.child_by_field_name("argument")?;
    loop {
        match argument.kind() {
            "parenthesized_expression" => argument = argument.named_child(0)?,
            "pointer_expression" => argument = argument.child_by_field_name("argument")?,
            _ => break,
        }
    }
    match argument.kind() {
        "this" => Some((String::new(), RECEIVER_HINT_VIA_CPP_THIS)),
        "identifier" => {
            let name = node_text(source, argument);
            declared_type(source, callee, &name).map(|t| (t, RECEIVER_HINT_VIA_CPP_DECLARED))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls_in(source: &str) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp grammar");
        let tree = parser.parse(source, None).expect("parse");
        let mut found = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "call_expression" {
                let props = receiver_props(source, n, "function");
                let get = |k: &str| {
                    props
                        .iter()
                        .find(|(name, _)| name == k)
                        .map_or("-".to_string(), |(_, v)| v.clone())
                };
                found.push((
                    n.start_byte(),
                    n.end_byte(),
                    get("receiver_hint"),
                    get("receiver_hint_via"),
                ));
            }
            let mut c = n.walk();
            stack.extend(n.children(&mut c));
        }
        found.sort_by_key(|(start, end, ..)| (*start, std::cmp::Reverse(*end)));
        found.into_iter().map(|(_, _, h, v)| (h, v)).collect()
    }

    fn pair(hint: &str, via: &str) -> (String, String) {
        (hint.to_string(), via.to_string())
    }

    #[test]
    fn reads_this_declared_and_qualified_receivers() {
        let src = "\
void f(const ns::Bloom<int>* pb, Umap& ru) {
    Local l;
    auto a = &l;
    pb->empty();
    ru.empty();
    l.size();
    a->size();
    this->go();
    (*this).go();
    other.size();
    ns::width(3);
    ::width(4);
    std::max<int>(1, 2);
    plain(5);
}";
        assert_eq!(
            calls_in(src),
            vec![
                pair("ns::Bloom", "cpp-declared"),
                pair("Umap", "cpp-declared"),
                pair("Local", "cpp-declared"),
                pair("-", "-"),
                pair("", "cpp-this"),
                pair("", "cpp-this"),
                pair("-", "-"),
                pair("ns", "cpp-qualifier"),
                pair("", "cpp-qualifier"),
                pair("std", "cpp-qualifier"),
                pair("-", "-"),
            ]
        );
    }

    #[test]
    fn an_inner_declaration_shadows_an_outer_one() {
        let src = "\
void f(A& x) {
    { B x; x.go(); }
    x.go();
    for (C& x : xs) { x.go(); }
    if (D* x = find()) { x->go(); }
    auto y = make();
    { Local y; y.go(); }
}";
        let hints: Vec<String> = calls_in(src).into_iter().map(|(h, _)| h).collect();
        assert_eq!(hints, ["B", "A", "C", "-", "D", "-", "Local"]);
    }

    #[test]
    fn a_catch_parameter_declares_its_name_in_its_handler_only() {
        let src = "\
void f(A& e) {
    try { g(); }
    catch (const ns::Err& e) { e.what(); }
    catch (Other o) { o.go(); }
    catch (...) { e.stop(); }
    e.after();
    catch_not(1);
}
void h() {
    try { g(); }
    catch (A a) { a.one(); }
    catch (B b) { a.two(); b.three(); }
}";
        let hints: Vec<String> = calls_in(src).into_iter().map(|(h, _)| h).collect();
        assert_eq!(
            hints,
            ["-", "ns::Err", "Other", "A", "A", "-", "-", "A", "-", "B"]
        );
    }

    #[test]
    fn a_template_parameter_is_no_receiver_type() {
        let src = "\
template <class T, typename U = Foo, int N, template <class> class C, typename... Ts>
void f(T& a, U& b, Foo& c, C<int>& d, Ts& e, Real& g) {
    a.go(); b.go(); c.go(); d.go(); e.go(); g.go();
}
struct S { template <class V> void h(V& v, T& t) { v.go(); t.go(); } };";
        let hints: Vec<String> = calls_in(src).into_iter().map(|(h, _)| h).collect();
        assert_eq!(hints, ["-", "-", "Foo", "-", "-", "Real", "-", "T"]);
    }

    #[test]
    fn a_declaring_form_hides_the_catch_parameter_it_redeclares() {
        // `Some(T)`: the form declares `e` with the type `T`; `None`: it declares `e`
        // with no type the source writes. The catch parameter `A& e` is never the receiver.
        let forms = [
            ("{ auto [e, c] = q; return e.m(); }", None),
            ("{ auto l = [e = B()]() { return e.m(); }; }", None),
            ("{ using ns::e; return e.m(); }", None),
            ("{\n#if X\n B e;\n#endif\n return e.m(); }", None),
            ("for (auto [e, c] : qs) { return e.m(); }", None),
            (
                "{ auto l = [](auto&... e) { return (e.m() + ...); }; }",
                None,
            ),
            ("if (B e; true) { return e.m(); }", Some("B")),
            ("switch (B e; 1) { default: return e.m(); }", Some("B")),
            ("{ L: B e; return e.m(); }", Some("B")),
            (
                "switch (1) { case 1: B e; break; default: return e.m(); }",
                Some("B"),
            ),
            ("for (B e; int x : xs) { return e.m(); }", Some("B")),
            ("{ B e [[maybe_unused]] = B(); return e.m(); }", Some("B")),
            ("{ auto l = []<B e>() { return e.m(); }; }", Some("B")),
            ("{ bool ok = requires (B e) { e.m(); }; }", Some("B")),
            ("{ B o; return e.m(); }", Some("A")),
        ];
        for (body, want) in forms {
            let src = format!("int f() {{ try {{ g(); }} catch (A& e) {{ {body} }} }}");
            let hints: Vec<(String, String)> = calls_in(&src)
                .into_iter()
                .filter(|(_, via)| via == "cpp-declared" || via == "-")
                .collect();
            let last = hints
                .last()
                .map(|(h, via)| (via == "cpp-declared").then_some(h.as_str()));
            assert_eq!(
                last.map(|h| h.map(str::to_string)),
                Some(want.map(str::to_string)),
                "{body}"
            );
        }
    }
}
