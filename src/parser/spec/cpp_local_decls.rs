// parser::spec::cpp_local_decls — the type names a function body declares for
// itself (issue #412).
//
// A name a block declares (`using other::Box;`, `typedef other::Box Box;`,
// `using Box = other::Box;`, a local `struct Box`) is the innermost scope C++ looks
// in, before any namespace or class around the function. The graph holds no node
// for a block, so the resolver cannot see it: a receiver whose type is written with
// such a name carries no hint (`cpp_receiver::written_type`), and its site stays
// open, as a template parameter's does.
//
// source: ISO/IEC 14882:2017 §6.3.3 (block scope), §6.4.1 (unqualified name lookup
// proceeds from the innermost scope outwards), §10.1 / §9.1 (a class declared in a
// block is a local class).

use tree_sitter::Node;

use crate::parser::node_text;

/// True when a block that encloses `at` declares, before `at`, a type named `name`
/// or brings names in with a using-directive (`using namespace n;`, which may
/// supply `name`).
/// precondition: `at` belongs to the tree of `source`.
/// postcondition: false when no enclosing `compound_statement` holds such a
/// declaration earlier than `at`; a declaration of a closed sibling block, one after
/// `at`, and one of another name never count.
pub(super) fn block_declares(source: &str, at: Node, name: &str) -> bool {
    let mut scope = at.parent();
    while let Some(s) = scope {
        if s.kind() == "compound_statement" && declares_in(source, s, at.start_byte(), name) {
            return true;
        }
        scope = s.parent();
    }
    false
}

fn declares_in(source: &str, block: Node, before: usize, name: &str) -> bool {
    let mut cursor = block.walk();
    let found = block
        .named_children(&mut cursor)
        .filter(|statement| statement.start_byte() < before)
        .any(|statement| introduces(source, statement, name));
    found
}

/// True when the statement `node` declares a type `name` (or a using-directive).
fn introduces(source: &str, node: Node, name: &str) -> bool {
    match node.kind() {
        "using_declaration" => using_introduces(source, node, name),
        "alias_declaration" => node
            .child_by_field_name("name")
            .is_some_and(|n| node_text(source, n) == name),
        "type_definition" => typedef_introduces(source, node, name),
        "declaration" => node
            .child_by_field_name("type")
            .is_some_and(|ty| class_named(source, ty, name, true)),
        _ => class_named(source, node, name, false),
    }
}

/// `using ns::Box;` introduces `Box`; `using namespace ns;` may introduce any name.
fn using_introduces(source: &str, node: Node, name: &str) -> bool {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    if children.iter().any(|c| c.kind() == "namespace") {
        return true;
    }
    children
        .iter()
        .rev()
        .find(|c| c.is_named())
        .is_some_and(|path| node_text(source, *path).rsplit("::").next() == Some(name))
}

/// `typedef T Box;` (any declarator that bears `Box`), or `typedef struct Box {..} B;`.
fn typedef_introduces(source: &str, node: Node, name: &str) -> bool {
    let mut cursor = node.walk();
    let declarators: Vec<Node> = node
        .children_by_field_name("declarator", &mut cursor)
        .collect();
    declarators.into_iter().any(|d| names_leaf(source, d, name))
        || node
            .child_by_field_name("type")
            .is_some_and(|ty| class_named(source, ty, name, true))
}

/// True when an identifier of `node`'s subtree is `name`.
fn names_leaf(source: &str, node: Node, name: &str) -> bool {
    if matches!(node.kind(), "type_identifier" | "identifier") {
        return node_text(source, node) == name;
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .any(|child| names_leaf(source, child, name));
    found
}

/// True when `node` is a class, struct, union or enum specifier named `name` that
/// declares it: one with a body, or (`needs_body` false) a forward declaration.
fn class_named(source: &str, node: Node, name: &str, needs_body: bool) -> bool {
    matches!(
        node.kind(),
        "struct_specifier" | "class_specifier" | "union_specifier" | "enum_specifier"
    ) && (!needs_body || node.child_by_field_name("body").is_some())
        && node
            .child_by_field_name("name")
            .is_some_and(|n| node_text(source, n) == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether the declaration of `Box` the block holds is seen from the call
    /// `probe()`.
    fn hides_box(src: &str) -> bool {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp grammar");
        let tree = parser.parse(src, None).expect("parse");
        let at = tree
            .root_node()
            .descendant_for_byte_range(
                src.find("probe").expect("probe"),
                src.find("probe").expect("probe") + 1,
            )
            .expect("node");
        block_declares(src, at, "Box")
    }

    #[test]
    fn a_using_a_typedef_an_alias_and_a_local_class_declare_the_name() {
        for decl in [
            "using other::Box;",
            "using ::other::Box;",
            "typedef other::Box Box;",
            "typedef other::Box* Box;",
            "typedef struct { int a; } Box;",
            "using Box = other::Box;",
            "struct Box { int m(); };",
            "class Box {};",
            "union Box { int a; };",
            "enum Box { A };",
            "enum class Box { A };",
            "struct Box { int m(); } b0;",
            "struct Box;",
            "using namespace other;",
        ] {
            let src = format!("void f() {{ {decl} probe(); }}");
            assert!(hides_box(&src), "{decl}");
        }
    }

    #[test]
    fn another_name_a_closed_block_or_a_later_declaration_declare_nothing() {
        for src in [
            "void f() { using other::Other; probe(); }",
            "void f() { typedef other::Box Boxes; probe(); }",
            "void f() { struct Other {}; probe(); }",
            "void f() { { using other::Box; } probe(); }",
            "void f() { probe(); using other::Box; }",
            "void f() { struct Box* p = nullptr; probe(); }",
            "void f() { Box b; probe(); }",
            "using other::Box; void f() { probe(); }",
        ] {
            assert!(!hides_box(src), "{src}");
        }
    }

    #[test]
    fn an_enclosing_block_declares_for_a_nested_one() {
        assert!(hides_box(
            "void f() { using other::Box; if (x) { while (y) { probe(); } } }"
        ));
        assert!(hides_box(
            "void f() { struct Box { void g() { probe(); } }; }"
        ));
    }
}
