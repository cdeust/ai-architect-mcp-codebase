// parser::spec::cpp_local_types: the declarations of a type name the caller's own
// body writes (issue #412, point 3).
//
// A receiver's type is looked up in the scopes of the caller by
// `resolver::calls::cpp_classes`. That lookup reads the classes of the graph, and
// the graph holds no node for a class, an alias or a `typedef` a function body
// declares, nor for a using-declaration of a block. `declares_type_name` names
// those forms: `cpp_declared::declared_type` marks the hint it reads
// `cpp-declared-local-name` when one of them declares the type's own first name
// before the receiver, and the resolver then reads the type by path suffix alone,
// as it did before #412.
//
// source: tree-sitter-cpp 0.23.4 node-types.json; each reader below names the
// nodes it reads.

use tree_sitter::Node;

use crate::parser::node_text;

/// True when a scope from `from_scope` out to the enclosing function declares
/// `name` as a type before the byte `before`: a using-declaration (`using
/// other::Box;`), an alias (`using Box = other::Box;`), a `typedef` or a class,
/// struct, union or enum declared in the body. The graph holds no node for any
/// of them, so a type written with that name names no class the graph can find
/// by scope.
pub(super) fn declares_type_name(
    source: &str,
    from_scope: Node,
    before: usize,
    name: &str,
) -> bool {
    let mut scope = Some(from_scope);
    while let Some(s) = scope {
        if s.kind() == "function_definition" {
            return false;
        }
        if scope_declares_type(source, s, before, name) {
            return true;
        }
        scope = s.parent();
    }
    false
}

/// True when a child of `scope` that starts at or before `before` (the receiver's
/// own declaration included: `struct Box { … } b;` declares the type too) declares
/// the type `name`.
/// A label, a `case` and a preprocessor branch are read through, as in `Reader::block`.
fn scope_declares_type(source: &str, scope: Node, before: usize, name: &str) -> bool {
    let mut cursor = scope.walk();
    let children: Vec<Node> = scope.named_children(&mut cursor).collect();
    children
        .into_iter()
        .filter(|child| child.start_byte() <= before)
        .any(|child| match child.kind() {
            "using_declaration" => using_declares(source, child, name),
            "alias_declaration" => field_is_name(source, child, "name", name),
            "type_definition" => declarator_names(source, child, name),
            // A declaration whose type is a class the body writes (`struct Box
            // { … };`), and one that declares the name itself: the grammar reads
            // `L: using other::Box;` and `L: typedef other::Box Box;` as a
            // declaration of `Box`, and a name a declaration of the body writes
            // is no class the scopes of the caller hold either way.
            "declaration" | "field_declaration" => {
                child
                    .child_by_field_name("type")
                    .is_some_and(|ty| specifier_is_name(source, ty, name))
                    || declarator_names(source, child, name)
            }
            "struct_specifier" | "class_specifier" | "union_specifier" | "enum_specifier" => {
                specifier_is_name(source, child, name)
            }
            "labeled_statement" | "case_statement" | "preproc_if" | "preproc_ifdef"
            | "preproc_elif" | "preproc_elifdef" | "preproc_else" => {
                scope_declares_type(source, child, before, name)
            }
            _ => false,
        })
}

/// True when a `using_declaration` names `name`: the `name` of its
/// qualified_identifier (`using other::Box;`), or its identifier. A
/// using-directive (`using namespace other;`) names nothing by itself.
pub(super) fn using_declares(source: &str, decl: Node, name: &str) -> bool {
    let mut cursor = decl.walk();
    let children: Vec<Node> = decl.named_children(&mut cursor).collect();
    children.into_iter().any(|c| {
        let mut last = c;
        while last.kind() == "qualified_identifier" {
            let Some(inner) = last.child_by_field_name("name") else {
                break;
            };
            last = inner;
        }
        matches!(last.kind(), "identifier" | "type_identifier") && node_text(source, last) == name
    })
}

/// True when `node` is a class, struct, union or enum specifier named `name`.
fn specifier_is_name(source: &str, node: Node, name: &str) -> bool {
    matches!(
        node.kind(),
        "struct_specifier" | "class_specifier" | "union_specifier" | "enum_specifier"
    ) && field_is_name(source, node, "name", name)
}

/// True when the `field` of `node` is written exactly `name`.
fn field_is_name(source: &str, node: Node, field: &str, name: &str) -> bool {
    node.child_by_field_name(field)
        .is_some_and(|n| node_text(source, n) == name)
}

/// True when a declaration declares `name`: an identifier of its declarators
/// (`typedef other::Box Box;`, `typedef other::Box *PBox, Box;`, and the
/// `other::Box` the grammar reads as the declarator of `L: using other::Box;`).
fn declarator_names(source: &str, decl: Node, name: &str) -> bool {
    let mut cursor = decl.walk();
    let declarators: Vec<Node> = decl
        .children_by_field_name("declarator", &mut cursor)
        .collect();
    declarators
        .into_iter()
        .any(|declarator| declarator_has_name(source, declarator, name))
}

/// True when a declarator declares `name`. Only the declarator's own structure
/// is read: the initial value of an `init_declarator` (`B e = B();`), the size
/// of an array and the parameters of a function declarator declare no name here.
fn declarator_has_name(source: &str, node: Node, name: &str) -> bool {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "field_identifier"
    ) && node_text(source, node) == name
    {
        return true;
    }
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let child = cursor.node();
        let declares = !matches!(
            (node.kind(), cursor.field_name()),
            ("init_declarator", Some("value"))
                | ("array_declarator", Some("size"))
                | ("function_declarator", Some("parameters"))
        );
        if child.is_named() && declares && declarator_has_name(source, child, name) {
            return true;
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}
