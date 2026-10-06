// parser::spec::cpp_unreadable: the statements before a call that could declare its receiver
// in a form `cpp_declared::Reader` cannot name (issue #412). source: ADR-9846.

use tree_sitter::Node;

use crate::parser::node_text;

/// True when a child of `scope` before the call (the one that holds it excluded)
/// is a statement `unreadable_statement` flags.
pub(super) fn scope_has_unreadable_statement(source: &str, of: (Node, Node), name: &str) -> bool {
    let (scope, call) = of;
    let mut cursor = scope.walk();
    let found = scope
        .named_children(&mut cursor)
        .take_while(|c| c.end_byte() <= call.start_byte())
        .any(|c| unreadable_statement(source, c, name));
    found
}

/// Statements that declare no name of the enclosing block, or whose declarations the
/// reader names (`Reader::block`).
const NO_DECLARATION: [&str; 21] = [
    "comment",
    "using_declaration",
    "type_definition",
    "alias_declaration",
    "static_assert_declaration",
    "namespace_alias_definition",
    "return_statement",
    "break_statement",
    "continue_statement",
    "goto_statement",
    "throw_statement",
    "if_statement",
    "while_statement",
    "do_statement",
    "for_statement",
    "for_range_loop",
    "switch_statement",
    "try_statement",
    "compound_statement",
    "co_return_statement",
    "co_yield_statement",
];

/// True when `stmt` could declare `name` in a form the reader cannot name. Statements
/// that hold statements of their own, and the header of one that encloses the call, are
/// entered.
fn unreadable_statement(source: &str, stmt: Node, name: &str) -> bool {
    match stmt.kind() {
        "labeled_statement" | "case_statement" | "preproc_if" | "preproc_ifdef"
        | "preproc_elif" | "preproc_elifdef" | "preproc_else" | "condition_clause"
        | "init_statement" => {
            let mut cursor = stmt.walk();
            let found = stmt
                .named_children(&mut cursor)
                .any(|c| unreadable_statement(source, c, name));
            found
        }
        "declaration" => declaration_is_opaque(source, stmt, name),
        kind if NO_DECLARATION.contains(&kind) => false,
        "expression_statement" => call_may_declare(source, stmt, name),
        "ERROR" => mentions(source, stmt, name) || has_macro_name(source, stmt),
        kind if kind == "identifier" || kind.ends_with("_declarator") => {
            declarator_is_macro(source, stmt) || mentions(source, stmt, name)
        }
        _ => mentions(source, stmt, name),
    }
}

/// A macro name: two characters or more, upper case, digits and `_` only.
fn is_macro_like(text: &str) -> bool {
    text.len() > 1
        && text
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && text.chars().any(|c| c.is_ascii_uppercase())
}

/// True when a `declaration` may declare `name` in a way the reader does not read: a parse
/// error that mentions it, a macro type, or a macro name in a declarator.
fn declaration_is_opaque(source: &str, decl: Node, name: &str) -> bool {
    let mut cursor = decl.walk();
    let macro_declarator = decl
        .children_by_field_name("declarator", &mut cursor)
        .any(|d| declarator_is_macro(source, d));
    let macro_type = decl
        .child_by_field_name("type")
        .is_some_and(|t| t.kind() == "type_identifier" && is_macro_like(&node_text(source, t)));
    macro_declarator || macro_type || (has_error(decl) && mentions(source, decl, name))
}

/// True when `node` holds an identifier `found` accepts, outside the fields `skip`.
fn any_identifier(node: Node, skip: &[&str], found: &impl Fn(Node) -> bool) -> bool {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "namespace_identifier"
    ) && found(node)
    {
        return true;
    }
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let skipped = cursor.field_name().is_some_and(|f| skip.contains(&f));
        if !skipped && any_identifier(cursor.node(), skip, found) {
            return true;
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}

/// True when a macro name is written in the declarator, whatever wraps it. The parameters,
/// the initial value and the size of an array name nothing.
fn declarator_is_macro(source: &str, node: Node) -> bool {
    any_identifier(node, &["parameters", "value", "size"], &|n| {
        n.kind() == "identifier" && is_macro_like(&node_text(source, n))
    })
}

/// True when a macro name is an identifier somewhere under `node`.
fn has_macro_name(source: &str, node: Node) -> bool {
    any_identifier(node, &[], &|n| {
        n.kind() == "identifier" && is_macro_like(&node_text(source, n))
    })
}

/// True when `name` is an identifier somewhere under `node`; a name before `::` in a
/// parse error (`B e ::`) is a `namespace_identifier`.
fn mentions(source: &str, node: Node, name: &str) -> bool {
    any_identifier(node, &[], &|n| node_text(source, n) == name)
}

/// True when `node` is or holds a parse error.
fn has_error(node: Node) -> bool {
    node.is_error() || node.has_error()
}

/// Kinds of argument that read a value and cannot be a declarator: `VAR(e.x)`.
const READING_ARGUMENT: [&str; 7] = [
    "field_expression",
    "subscript_expression",
    "string_literal",
    "raw_string_literal",
    "number_literal",
    "char_literal",
    "concatenated_string",
];

/// The operand an assignment, a subscript or a dereference starts with.
fn leftmost_operand(node: Node) -> Node {
    let mut node = node;
    loop {
        let next = match node.kind() {
            "assignment_expression" => node.child_by_field_name("left"),
            "subscript_expression" | "pointer_expression" => node.child_by_field_name("argument"),
            _ => None,
        };
        match next {
            Some(inner) => node = inner,
            None => return node,
        }
    }
}

/// True when the call an expression statement starts with, or a call it calls
/// (`DECLARE(B, e)(B());`), may declare `name`; so may a statement that is a macro name.
fn call_may_declare(source: &str, stmt: Node, name: &str) -> bool {
    let Some(statement) = stmt.named_child(0) else {
        return false;
    };
    let mut call = leftmost_operand(statement);
    if call.kind() == "identifier" {
        return call.id() == statement.id() && is_macro_like(&node_text(source, call));
    }
    while call.kind() == "call_expression" {
        if call_declares(source, call, name) {
            return true;
        }
        match call.child_by_field_name("function") {
            Some(callee) => call = callee,
            None => return false,
        }
    }
    false
}

/// A call declares `name` when it has no argument and a macro name for callee, or when an
/// argument is a macro name or mentions the name and is not a plain read.
fn call_declares(source: &str, call: Node, name: &str) -> bool {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return false;
    };
    if arguments.named_child_count() == 0 {
        return call
            .child_by_field_name("function")
            .is_some_and(|f| f.kind() == "identifier" && is_macro_like(&node_text(source, f)));
    }
    let mut cursor = arguments.walk();
    let found = arguments.named_children(&mut cursor).any(|a| {
        is_macro_operand(source, a)
            || (!READING_ARGUMENT.contains(&a.kind()) && mentions(source, a, name))
    });
    found
}

/// A macro name, alone or under `&`, `*` or parentheses.
fn is_macro_operand(source: &str, node: Node) -> bool {
    let mut node = node;
    while matches!(
        node.kind(),
        "pointer_expression" | "parenthesized_expression"
    ) {
        let inner = node
            .child_by_field_name("argument")
            .or_else(|| node.named_child(0));
        match inner {
            Some(next) => node = next,
            None => return false,
        }
    }
    node.kind() == "identifier" && is_macro_like(&node_text(source, node))
}
