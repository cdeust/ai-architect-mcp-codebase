// parser::spec::cpp_unreadable: the statements before a call that could declare its receiver
// in a form `cpp_declared::Reader` cannot name (issue #412). source: ADR-9847.

use tree_sitter::Node;

use crate::parser::node_text;

/// True when a child of `scope` before the call (the one that holds it excluded) is
/// opaque. The callee of a call that holds the call in its arguments is skipped.
pub(super) fn scope_has_unreadable_statement(source: &str, of: (Node, Node), name: &str) -> bool {
    let (scope, call) = of;
    let callee = scope
        .child_by_field_name("function")
        .filter(|_| scope.kind() == "call_expression");
    let mut cursor = scope.walk();
    let found = scope
        .named_children(&mut cursor)
        .take_while(|c| c.end_byte() <= call.start_byte())
        .filter(|c| callee.is_none_or(|f| f.id() != c.id()))
        .any(|c| is_opaque(source, c, name));
    found
}

/// Statements that declare nothing the call can read: a scope closed before it, a type, a
/// jump.
const CLOSED: [&str; 20] = [
    "comment",
    "type_definition",
    "alias_declaration",
    "static_assert_declaration",
    "namespace_alias_definition",
    "return_statement",
    "break_statement",
    "continue_statement",
    "goto_statement",
    "throw_statement",
    "co_return_statement",
    "co_yield_statement",
    "if_statement",
    "while_statement",
    "do_statement",
    "for_statement",
    "for_range_loop",
    "switch_statement",
    "try_statement",
    "compound_statement",
];

/// True when `node` may declare `name` in a form the reader cannot name: a macro name, or
/// the name written where it is not read, anywhere in it. A declaration the parser read
/// whole is read by the reader, which names a `name` it writes.
fn is_opaque(source: &str, node: Node, name: &str) -> bool {
    match node.kind() {
        kind if CLOSED.contains(&kind) => false,
        "preproc_include" | "preproc_def" | "preproc_function_def" | "preproc_call" => true,
        "declaration" if !node.has_error() => hits(source, node, ""),
        _ => hits(source, node, name),
    }
}

/// True when `node` holds a hit. The initial value, the size of an array and the parameters
/// of a declared function name nothing.
fn hits(source: &str, node: Node, name: &str) -> bool {
    if is_hit(source, node, name) {
        return true;
    }
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let skipped = !node.has_error()
            && matches!(
                (node.kind(), cursor.field_name()),
                ("init_declarator", Some("value"))
                    | ("array_declarator", Some("size"))
                    | ("function_declarator", Some("parameters"))
            );
        if !skipped && hits(source, cursor.node(), name) {
            return true;
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}

/// A macro name, or `name` outside a read (`e.x`, `e->x`, `e[i]`).
fn is_hit(source: &str, node: Node, name: &str) -> bool {
    matches!(
        node.kind(),
        "identifier" | "type_identifier" | "namespace_identifier" | "statement_identifier"
    ) && {
        let text = node_text(source, node);
        is_macro_like(&text) || (!name.is_empty() && text == name && !is_read(node))
    }
}

/// True when `node` is the object of a member access or of a subscript.
fn is_read(node: Node) -> bool {
    node.parent().is_some_and(|p| {
        matches!(p.kind(), "field_expression" | "subscript_expression")
            && p.child_by_field_name("argument")
                .is_some_and(|a| a.id() == node.id())
    })
}

/// A macro name: two characters or more, upper case, digits and `_` only.
fn is_macro_like(text: &str) -> bool {
    text.len() > 1
        && text
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && text.chars().any(|c| c.is_ascii_uppercase())
}
