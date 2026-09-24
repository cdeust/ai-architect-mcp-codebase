// parser::spec::rust_type_scope: does the file show where the return type of a
// free function comes from? (issues #348, #349)
//
// The resolver keeps the last segment of a type, so a name that may come from
// somewhere the file does not show (a glob import of an external crate, the
// prelude, a macro) could be matched to any repository type of that name. In
// Rust a bare name is resolved in the module of the function: the module's own
// definitions and imports, its glob imports, then the prelude; the imports and
// definitions of another module, the root included, are not seen. This module
// applies that rule to one file and accepts the name only when the file
// shows its source:
//   - a struct, enum or union of that name in the module of the function (it
//     shadows every glob);
//   - an explicit `use` of that name in the module (it wins over a glob);
//   - otherwise only `use super::*;` globs, and then the same test one module
//     up, since `super::*` brings the parent's definitions and imports.
// Anything else (a glob of another path, no import at all, the root's
// `super`) is declined. The safe direction is to decline.

use tree_sitter::Node;

use crate::parser::node_text;

/// source: tree-sitter-rust 0.24.2 src/node-types.json: `mod_item` holds its
/// items in a `declaration_list`; the root is a `source_file`.
const MODULE_BODY_KIND: &str = "declaration_list";
const ROOT_KIND: &str = "source_file";

/// Upper bound on how many `super::*` hops are followed; a file nests far less.
const MAX_HOPS: usize = 16;

/// True when the file shows where the type `ty` named in the signature of
/// `function` comes from, as described in the module header.
pub(super) fn type_source_is_shown(source: &str, function: Node, ty: &str) -> bool {
    match module_body_of(function) {
        Some(body) => shown_in(source, body, ty, MAX_HOPS),
        None => false,
    }
}

/// The `declaration_list` of the enclosing `mod`, or the root, of `node`.
fn module_body_of(node: Node) -> Option<Node> {
    let mut current = node.parent();
    while let Some(n) = current {
        let is_mod_body =
            n.kind() == MODULE_BODY_KIND && n.parent().is_some_and(|p| p.kind() == "mod_item");
        if is_mod_body || n.kind() == ROOT_KIND {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

fn shown_in(source: &str, body: Node, ty: &str, hops: usize) -> bool {
    let mut globs_to_parent = false;
    let mut other_glob = false;
    let mut cursor = body.walk();
    for item in body.named_children(&mut cursor) {
        match item.kind() {
            "struct_item" | "enum_item" | "union_item" if declares(source, item, ty) => {
                return true;
            }
            "use_declaration" => {
                if binds_explicitly(source, item, ty) {
                    return true;
                }
                for glob in wildcards(item) {
                    match node_text(source, glob).replace(' ', "").as_str() {
                        "super::*" => globs_to_parent = true,
                        "self::*" => {}
                        _ => other_glob = true,
                    }
                }
            }
            _ => {}
        }
    }
    if other_glob || !globs_to_parent || hops == 0 {
        return false;
    }
    module_body_of(body).is_some_and(|parent| shown_in(source, parent, ty, hops - 1))
}

fn declares(source: &str, node: Node, name: &str) -> bool {
    node.child_by_field_name("name")
        .is_some_and(|n| node_text(source, n) == name)
}

/// Every `use_wildcard` node of a `use` declaration.
fn wildcards(use_declaration: Node) -> Vec<Node> {
    let mut found = Vec::new();
    let mut stack = vec![use_declaration];
    while let Some(node) = stack.pop() {
        if node.kind() == "use_wildcard" {
            found.push(node);
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    found
}

/// True when `use_declaration` binds `ty` by name: the word `ty` appears in it
/// outside its glob parts (`use ext::Set::*;` binds no `Set`) and outside its
/// `as` clauses (`use a::Set as Other;` binds `Other`).
fn binds_explicitly(source: &str, use_declaration: Node, ty: &str) -> bool {
    let mut skipped: Vec<(usize, usize)> = wildcards(use_declaration)
        .iter()
        .map(|n| (n.start_byte(), n.end_byte()))
        .collect();
    let mut stack = vec![use_declaration];
    while let Some(node) = stack.pop() {
        if node.kind() == "use_as_clause" {
            skipped.push((node.start_byte(), node.end_byte()));
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let text = node_text(source, use_declaration);
    let base = use_declaration.start_byte();
    let kept: String = text
        .char_indices()
        .filter(|(i, _)| !skipped.iter().any(|(s, e)| (*s..*e).contains(&(base + i))))
        .map(|(_, c)| c)
        .collect();
    kept.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w == ty)
}

#[cfg(test)]
mod tests {
    use super::{type_source_is_shown, wildcards};
    use tree_sitter::{Node, Parser, Tree};

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("language");
        parser.parse(src, None).expect("parse")
    }

    fn first_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        let found = node
            .children(&mut cursor)
            .find_map(|c| first_of_kind(c, kind));
        found
    }

    // source: every glob form below, parsed by tree-sitter-rust 0.24.2
    // (Cargo.lock), yields a `use_wildcard` node, and the free function that
    // returns `Set`, with nothing in the file showing where `Set` comes from,
    // is declined for each of them.
    const GLOB_FORMS: [&str; 8] = [
        "use a::*;\nfn make() -> Set { todo!() }",
        "use a::{b::*, c};\nfn make() -> Set { todo!() }",
        "use a::{self, b::{c::*}};\nfn make() -> Set { todo!() }",
        "pub use a::*;\nfn make() -> Set { todo!() }",
        "use crate::x::*;\nfn make() -> Set { todo!() }",
        "use super::*;\nfn make() -> Set { todo!() }",
        "use self::inner::*;\nfn make() -> Set { todo!() }",
        "mod tests {\n    #[cfg(test)]\n    use super::*;\n    fn make() -> Set { todo!() }\n}",
    ];

    #[test]
    fn every_glob_form_is_a_use_wildcard_and_declines_a_type_it_may_hide() {
        for src in GLOB_FORMS {
            let tree = parse(src);
            let root = tree.root_node();
            let mut found = Vec::new();
            let mut stack = vec![root];
            while let Some(n) = stack.pop() {
                if n.kind() == "use_declaration" {
                    found.extend(wildcards(n));
                }
                let mut cursor = n.walk();
                stack.extend(n.named_children(&mut cursor));
            }
            assert!(!found.is_empty(), "no `use_wildcard` node in {src}");
            let function = first_of_kind(root, "function_item").expect("a function");
            assert!(
                !type_source_is_shown(src, function, "Set"),
                "a glob form accepted a type it may hide: {src}"
            );
        }
    }

    #[test]
    fn a_definition_or_an_explicit_import_in_the_module_shows_the_source() {
        for src in [
            "use a::*;\nstruct Set;\nfn make() -> Set { todo!() }",
            "use a::*;\nenum Set { A }\nfn make() -> Set { todo!() }",
            "use a::{b::*, Set};\nfn make() -> Set { todo!() }",
            "use a::Set;\nfn make() -> Set { todo!() }",
            "mod tests {\n    use super::*;\n    fn make() -> Set { todo!() }\n}\nstruct Set;",
        ] {
            let tree = parse(src);
            let function = first_of_kind(tree.root_node(), "function_item")
                .or_else(|| {
                    let root = tree.root_node();
                    let module = first_of_kind(root, "mod_item")?;
                    first_of_kind(module, "function_item")
                })
                .expect("a function");
            assert!(type_source_is_shown(src, function, "Set"), "{src}");
        }
    }

    #[test]
    fn a_glob_over_an_enum_or_an_as_clause_binds_no_name() {
        for src in [
            "use ext::Set::*;\nfn make() -> Set { todo!() }",
            "use ext::Real as Set;\nfn make() -> Set { todo!() }",
            "use ext::Set as Other;\nfn make() -> Set { todo!() }",
        ] {
            let tree = parse(src);
            let function = first_of_kind(tree.root_node(), "function_item").expect("a function");
            assert!(!type_source_is_shown(src, function, "Set"), "{src}");
        }
    }
}
