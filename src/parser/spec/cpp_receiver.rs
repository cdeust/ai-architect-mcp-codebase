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
//   variable or a condition declares with a type written in the source (`Bloom*
//   p`) or a catch clause (`catch (const E& e)`, in its handler only); the
//   hint is that type without qualifier, pointer, reference or generic arguments. A name declared with `auto`, a name no enclosing scope
//   declares (a field, a global), a type that is a template parameter of an
//   enclosing template (`template <class T> .. T& t`: not a class) or any other
//   expression gives no hint. A type whose first name an enclosing block declares
//   itself (`using other::Box;`, `typedef`, a local `struct Box`:
//   `cpp_local_decls`) is a `cpp-declared-block` hint: the graph holds no node for
//   a block, so the resolver cannot tell which class the name designates.
// - `cpp-qualifier`: the callee is written `a::b::f`; the hint is `a::b`.
//
// source: tree-sitter-cpp 0.23.4 node-types.json (field_expression.argument,
// declaration.{type,declarator}, parameter_declaration, for_range_loop,
// condition_clause.value, qualified_identifier.scope).

use tree_sitter::Node;

use crate::graph_store::{
    RECEIVER_HINT_VIA_CPP_DECLARED, RECEIVER_HINT_VIA_CPP_DECLARED_IN_BLOCK,
    RECEIVER_HINT_VIA_CPP_QUALIFIER, RECEIVER_HINT_VIA_CPP_THIS,
};
use crate::parser::generic_args::strip_generic_groups;
use crate::parser::node_text;

use super::cpp_local_decls::block_declares;

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
            let written = declared_type(source, callee, &name)?;
            let via = if written.in_block {
                RECEIVER_HINT_VIA_CPP_DECLARED_IN_BLOCK
            } else {
                RECEIVER_HINT_VIA_CPP_DECLARED
            };
            Some((written.name, via))
        }
        _ => None,
    }
}

/// A type as a declaration writes it, and whether a block that encloses the
/// declaration declares its first name itself (`cpp_local_decls`).
struct Written {
    name: String,
    in_block: bool,
}

/// The type the innermost enclosing scope declares for `name`, read at `at`.
/// The search stops at the enclosing function: a global or a field is not read.
fn declared_type(source: &str, at: Node, name: &str) -> Option<Written> {
    let mut scope = at.parent();
    while let Some(s) = scope {
        if let Some(found) = binding_in(source, s, (at, name)) {
            return found;
        }
        if s.kind() == "function_definition" {
            return None;
        }
        scope = s.parent();
    }
    None
}

/// `Some(type)` when `scope` declares `name` before `at` (the last such
/// declaration wins); the inner `None` is a declaration whose type the source
/// does not write (`auto`). `None` when `scope` does not declare it.
fn binding_in(source: &str, scope: Node, at: (Node, &str)) -> Option<Option<Written>> {
    let (call, name) = at;
    let mut found = None;
    for decl in declarations_of(scope) {
        if decl.start_byte() >= call.start_byte() {
            continue;
        }
        if declares(source, decl, name) {
            found = Some(written_type(source, decl));
        }
    }
    found
}

/// The nodes of `scope` that carry a `type` and a `declarator`.
fn declarations_of(scope: Node) -> Vec<Node> {
    let mut out = Vec::new();
    match scope.kind() {
        "function_definition" | "lambda_expression" => {
            if let Some(parameters) = parameters_of(scope) {
                let mut cursor = parameters.walk();
                out.extend(parameters.named_children(&mut cursor).filter(|p| {
                    matches!(
                        p.kind(),
                        "parameter_declaration" | "optional_parameter_declaration"
                    )
                }));
            }
        }
        "for_range_loop" => out.push(scope),
        // `catch (const E& e) { .. }`: the parameter is in scope in the handler only.
        "catch_clause" => out.extend(
            scope
                .child_by_field_name("parameters")
                .into_iter()
                .flat_map(|list| {
                    let mut cursor = list.walk();
                    list.named_children(&mut cursor)
                        .filter(|p| p.kind() == "parameter_declaration")
                        .collect::<Vec<_>>()
                }),
        ),
        "if_statement" | "while_statement" | "switch_statement" => {
            let value = scope
                .child_by_field_name("condition")
                .and_then(|c| c.child_by_field_name("value"))
                .filter(|v| v.kind() == "declaration");
            out.extend(value);
        }
        _ => {
            let mut cursor = scope.walk();
            out.extend(
                scope
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "declaration"),
            );
        }
    }
    out
}

fn parameters_of(callable: Node) -> Option<Node> {
    let mut declarator = callable.child_by_field_name("declarator");
    while let Some(d) = declarator {
        if let Some(parameters) = d.child_by_field_name("parameters") {
            return Some(parameters);
        }
        declarator = inner_declarator(d);
    }
    None
}

/// The declarator a wrapping declarator (pointer, reference, array, init,
/// parenthesis) applies to. A reference declarator names it without a field.
fn inner_declarator(node: Node) -> Option<Node> {
    node.child_by_field_name("declarator")
        .or_else(|| node.named_child(u32::try_from(node.named_child_count().checked_sub(1)?).ok()?))
}

fn declares(source: &str, decl: Node, name: &str) -> bool {
    let mut cursor = decl.walk();
    let declarators: Vec<Node> = decl
        .children_by_field_name("declarator", &mut cursor)
        .collect();
    declarators
        .into_iter()
        .any(|d| declarator_name(source, d).as_deref() == Some(name))
}

fn declarator_name(source: &str, declarator: Node) -> Option<String> {
    let mut node = declarator;
    loop {
        match node.kind() {
            "identifier" => return Some(node_text(source, node)),
            "init_declarator"
            | "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "parenthesized_declarator" => node = inner_declarator(node)?,
            _ => return None,
        }
    }
}

/// The class a declaration's `type` names, as written: `Bloom` of `const
/// Bloom*`, `ns::Bloom` of `ns::Bloom&`, `Map` of `Map<K, V>`. `auto`,
/// `decltype` and the builtin types give `None`.
fn written_type(source: &str, decl: Node) -> Option<Written> {
    let ty = decl.child_by_field_name("type")?;
    let text = match ty.kind() {
        "type_identifier" | "qualified_identifier" | "template_type" => node_text(source, ty),
        "struct_specifier" | "class_specifier" | "union_specifier" => {
            node_text(source, ty.child_by_field_name("name")?)
        }
        _ => return None,
    };
    let name = plain_type_text(&text);
    let first = name.split("::").next().unwrap_or(&name);
    if name.is_empty() || is_template_parameter(source, decl, first) {
        return None;
    }
    let in_block = block_declares(source, decl, first);
    Some(Written { name, in_block })
}

/// True when `name` is a parameter of a template that encloses `at`
/// (`template <class T>`, `typename... Ts`, `template <class> class C`): a type
/// written with it names no class of the repository.
fn is_template_parameter(source: &str, at: Node, name: &str) -> bool {
    let mut scope = at.parent();
    while let Some(s) = scope {
        if s.kind() == "template_declaration" {
            if let Some(list) = s.child_by_field_name("parameters") {
                if template_parameter_names(source, list)
                    .iter()
                    .any(|n| n == name)
                {
                    return true;
                }
            }
        }
        scope = s.parent();
    }
    false
}

/// The names a `template_parameter_list` declares for types: `T` of `class T`,
/// `typename... Ts` or `class T = Default`, and `C` of `template <class> class
/// C`. A non-type parameter (`int N`) names no type.
fn template_parameter_names(source: &str, list: Node) -> Vec<String> {
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter_map(|parameter| type_parameter_name(source, parameter))
        .collect()
}

/// The first `type_identifier` of a type parameter is its name (a default type
/// follows it).
fn type_parameter_name(source: &str, parameter: Node) -> Option<String> {
    match parameter.kind() {
        "type_parameter_declaration"
        | "variadic_type_parameter_declaration"
        | "optional_type_parameter_declaration" => {
            let mut cursor = parameter.walk();
            let name = parameter
                .named_children(&mut cursor)
                .find(|c| c.kind() == "type_identifier")
                .map(|c| node_text(source, c));
            name
        }
        "template_template_parameter_declaration" => {
            let mut cursor = parameter.walk();
            let name = parameter
                .named_children(&mut cursor)
                .find_map(|c| type_parameter_name(source, c));
            name
        }
        _ => None,
    }
}

/// `text` without its `<...>` groups and whitespace.
fn plain_type_text(text: &str) -> String {
    strip_generic_groups(text)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
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
    fn a_type_a_block_declares_is_a_block_hint() {
        let src = "\
void f(Box& p) {
    using other::Box;
    Box a;
    a.go();
    p.go();
    { struct Own {}; Own o; o.go(); }
    { Own o; o.go(); }
    typedef Real Alias;
    Alias c;
    c.go();
    Box late;
    late.go();
}";
        assert_eq!(
            calls_in(src),
            vec![
                pair("Box", "cpp-declared-block"),
                pair("Box", "cpp-declared"),
                pair("Own", "cpp-declared-block"),
                pair("Own", "cpp-declared"),
                pair("Alias", "cpp-declared-block"),
                pair("Box", "cpp-declared-block"),
            ]
        );
    }
}
