// parser::spec::cpp_unreadable — the statements before a call that could declare its receiver
// in a form `cpp_declared::Reader` cannot name (issue #412).
//
// The type of a catch parameter, which `main` never read, is used only when no statement
// between the handler and the call is flagged here. The list is of the kinds known not to
// declare, never of the kinds known to: a statement of any other kind that mentions the
// name, a declaration or an expression statement that looks like a declaring macro, and
// a parse error stand for a declaration the preprocessor or the compiler would read.

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
/// reader names (`Reader::block`). Any other statement that mentions the name could
/// declare it in a form the reader does not know (a macro `DECLARE(B, e);`, a parse
/// error): this list is of the kinds known not to, never of the kinds known to.
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

/// True when `stmt` could declare `name` in a form the reader cannot name: a
/// statement of a kind outside `NO_DECLARATION` that mentions the name, a
/// `declaration` the reader cannot fully read (`declaration_is_opaque`) and an
/// expression statement that looks like a declaring macro (`call_may_declare`). A
/// label, a `case` and a preprocessor branch hold statements of their own.
fn unreadable_statement(source: &str, stmt: Node, name: &str) -> bool {
    match stmt.kind() {
        "labeled_statement" | "case_statement" | "preproc_if" | "preproc_ifdef"
        | "preproc_elif" | "preproc_elifdef" | "preproc_else" => {
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

/// True when a `declaration` may declare `name` in a way `Reader::declaration` does not
/// read: it holds a parse error that mentions the name (`B e BRACES;`), its type is a
/// macro name (`DECL_E if (1) {`, read as a declaration of a variable `if`), or one of its
/// declarators is a bare macro name (`B DECL_E;`, `B NAMED(e);`), which stands for any
/// declarator the preprocessor writes. `B e(y);` is not opaque: the reader names `e`.
fn declaration_is_opaque(source: &str, decl: Node, name: &str) -> bool {
    let mut cursor = decl.walk();
    let macro_declarator = decl
        .children_by_field_name("declarator", &mut cursor)
        .any(|d| {
            let named = if d.kind() == "function_declarator" {
                d.child_by_field_name("declarator")
            } else {
                Some(d)
            };
            named.is_some_and(|n| n.kind() == "identifier" && is_macro_like(&node_text(source, n)))
        });
    let macro_type = decl
        .child_by_field_name("type")
        .is_some_and(|t| t.kind() == "type_identifier" && is_macro_like(&node_text(source, t)));
    macro_declarator || macro_type || (has_error(decl) && mentions(source, decl, name))
}

/// True when a macro name is an identifier somewhere under `node`.
fn has_macro_name(source: &str, node: Node) -> bool {
    if node.kind() == "identifier" && is_macro_like(&node_text(source, node)) {
        return true;
    }
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| has_macro_name(source, c));
    found
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

/// True when the call an expression statement starts with may be a macro that
/// declares `name`: its callee is a macro name and it has no argument
/// (`DECLARE_ALL();`), or one argument that is not a plain read mentions the name
/// (`VAR(B* e);`, `VAR(B& e = b);`, `DECLARE(B, e);`). A statement that is only a macro
/// name (`DECL_E` before a `return`) stands for any declaration.
fn call_may_declare(source: &str, stmt: Node, name: &str) -> bool {
    let Some(first) = stmt.named_child(0) else {
        return false;
    };
    if first.kind() == "identifier" {
        return is_macro_like(&node_text(source, first));
    }
    if first.kind() != "call_expression" {
        return false;
    }
    let Some(arguments) = first.child_by_field_name("arguments") else {
        return false;
    };
    let macro_callee = first
        .child_by_field_name("function")
        .is_some_and(|f| f.kind() == "identifier" && is_macro_like(&node_text(source, f)));
    if arguments.named_child_count() == 0 {
        return macro_callee;
    }
    let mut cursor = arguments.walk();
    let found = arguments
        .named_children(&mut cursor)
        .any(|a| !READING_ARGUMENT.contains(&a.kind()) && mentions(source, a, name));
    found
}

/// True when `name` is an identifier somewhere under `node`; a name before `::` in a
/// parse error (`B e ::`) is a `namespace_identifier`.
fn mentions(source: &str, node: Node, name: &str) -> bool {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "namespace_identifier"
    ) && node_text(source, node) == name
    {
        return true;
    }
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| mentions(source, c, name));
    found
}
