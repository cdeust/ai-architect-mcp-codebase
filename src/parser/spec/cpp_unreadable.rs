// parser::spec::cpp_unreadable: the statements before a call that could declare its receiver
// in a form `cpp_declared::Reader` cannot name (issue #412). source: ADR-9847.

use tree_sitter::Node;

use crate::parser::node_text;

/// What marks a byte the mask erased in the text the walkers read.
const ERASED: u8 = 0x0c;

/// `blanked` (the rewrite of `original`, same length) with a mark on each byte it erased.
pub(super) fn mark_erased(original: &str, blanked: &str) -> String {
    let bytes = original
        .bytes()
        .zip(blanked.bytes())
        .map(|(o, b)| if o != b { ERASED } else { b })
        .collect();
    String::from_utf8(bytes).unwrap_or_else(|_| blanked.to_string())
}

/// True when a child of `scope` before the call (the one that holds it excluded) is
/// opaque, or when the mask erased a token there. The callee of a call that holds the
/// call in its arguments, and the fields that name nothing, are skipped.
pub(super) fn scope_has_unreadable_statement(source: &str, of: (Node, Node), name: &str) -> bool {
    let (scope, call) = of;
    if source[scope.start_byte()..call.start_byte()].contains(char::from(ERASED)) {
        return true;
    }
    let mut cursor = scope.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let child = cursor.node();
        if child.end_byte() > call.start_byte() {
            return false;
        }
        let field = cursor.field_name();
        let callee = scope.kind() == "call_expression" && field == Some("function");
        if child.is_named()
            && !callee
            && !names_nothing(scope, field)
            && is_opaque(source, child, name)
        {
            return true;
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}

/// A field of a well formed node that declares nothing: the initial value, the size of an
/// array, the parameters of a declared function, the test of an `#if`.
fn names_nothing(node: Node, field: Option<&str>) -> bool {
    !node.has_error()
        && matches!(
            (node.kind(), field),
            ("init_declarator", Some("value"))
                | ("array_declarator", Some("size"))
                | ("function_declarator", Some("parameters"))
                | ("preproc_ifdef" | "preproc_elifdef", Some("name"))
                | ("preproc_if" | "preproc_elif", Some("condition"))
        )
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
        "declaration" if !node.has_error() => hits(source, node, ""),
        _ => hits(source, node, name),
    }
}

/// True when `node` holds a hit; a directive is one. The fields `names_nothing` lists are
/// skipped.
fn hits(source: &str, node: Node, name: &str) -> bool {
    if matches!(
        node.kind(),
        "preproc_include" | "preproc_def" | "preproc_function_def" | "preproc_call"
    ) || is_hit(source, node, name)
    {
        return true;
    }
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let skipped = names_nothing(node, cursor.field_name());
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
        "identifier"
            | "type_identifier"
            | "namespace_identifier"
            | "statement_identifier"
            | "field_identifier"
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
