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

/// What the file shows about the source of a type name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Shown {
    /// Nothing: the name may come from anywhere.
    No,
    /// A definition of the module (or of a parent reached through `super::*`).
    Local,
    /// An explicit `use` of a path of the same crate (`crate::`, `self::`,
    /// `super::`), with the whole path as written (`crate::shapes::Set`). In a
    /// test, bench, example or bin target, `crate` names that target, not the
    /// library, so the resolver reads the path (issue #357).
    LocalImport(String),
    /// An explicit `use` of a path that starts with this other name: a crate
    /// of the repository or a foreign one, which the file cannot tell.
    Import(String),
}

/// What the file shows about where the type `ty` named in the signature of
/// `function` comes from, as described in the module header.
pub(super) fn type_source(source: &str, function: Node, ty: &str) -> Shown {
    match module_body_of(function) {
        Some(body) => shown_in(source, body, ty, MAX_HOPS),
        None => Shown::No,
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

fn shown_in(source: &str, body: Node, ty: &str, hops: usize) -> Shown {
    let mut globs_to_parent = false;
    let mut other_glob = false;
    let mut imports: Vec<(String, String)> = Vec::new();
    let mut cursor = body.walk();
    for item in body.named_children(&mut cursor) {
        match item.kind() {
            "struct_item" | "enum_item" | "union_item" if declares(source, item, ty) => {
                return Shown::Local;
            }
            "use_declaration" => {
                imports.extend(bound_roots(source, item, ty));
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
    imports.sort();
    imports.dedup();
    let mut roots: Vec<&str> = imports.iter().map(|(root, _)| root.as_str()).collect();
    roots.dedup();
    match (roots.as_slice(), imports.as_slice()) {
        ([], _) => {}
        ([root], [(_, path)]) if ["crate", "self", "super"].contains(root) => {
            return Shown::LocalImport(path.clone())
        }
        // Two paths of the same crate for one name: in doubt.
        ([root], _) if ["crate", "self", "super"].contains(root) => return Shown::No,
        ([root], _) => return Shown::Import((*root).to_string()),
        // Two explicit imports of the name from different roots: in doubt.
        _ => return Shown::No,
    }
    if other_glob || !globs_to_parent || hops == 0 {
        return Shown::No;
    }
    module_body_of(body).map_or(Shown::No, |parent| shown_in(source, parent, ty, hops - 1))
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

/// `(first path segment, whole path)` of every way `use_declaration` binds
/// exactly the name `ty` (not an alias, not a glob, not a path segment that is
/// not the last): `use ext::Set;` gives `(ext, ext::Set)`, `use ext::{Set,
/// Other};` gives `(ext, ext::Set)`, `use ext::Set::{self, A};` gives
/// `(ext, ext::Set)`, `use ext::Set::Variant;` and `use Set::{A, B};` give none.
fn bound_roots(source: &str, use_declaration: Node, ty: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(argument) = use_declaration.child_by_field_name("argument") {
        collect_bindings(source, argument, None, &mut out);
    }
    out.into_iter()
        .filter(|b| b.name == ty)
        .map(|b| (b.root, b.path))
        .collect()
}

/// One name a use tree binds.
struct Binding {
    name: String,
    root: String,
    path: String,
}

/// The enclosing path of a node of a use tree: its first segment and the whole
/// prefix as written, spaces removed.
#[derive(Clone, Copy)]
struct Prefix<'p> {
    root: &'p str,
    path: &'p str,
}

fn joined(prefix: Option<Prefix>, rest: &str) -> String {
    prefix.map_or_else(|| rest.to_string(), |p| format!("{}::{rest}", p.path))
}

/// The bindings of a `scoped_use_list` (`a::b::{Set, c::D}`): each child of the
/// list under the enclosing path, and `self` as the last segment of the path.
fn collect_list_bindings(source: &str, node: Node, prefix: Option<Prefix>, out: &mut Vec<Binding>) {
    let path = node.child_by_field_name("path");
    let root = match (prefix, path) {
        (Some(p), _) => p.root.to_string(),
        (None, Some(p)) => leftmost(source, p),
        (None, None) => return,
    };
    let whole = match path {
        Some(p) => joined(prefix, &node_text(source, p).replace(' ', "")),
        None => prefix.map(|p| p.path.to_string()).unwrap_or_default(),
    };
    let Some(list) = node.child_by_field_name("list") else {
        return;
    };
    let inner = Some(Prefix {
        root: &root,
        path: &whole,
    });
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        if child.kind() != "self" {
            collect_bindings(source, child, inner, out);
            continue;
        }
        // `use a::Set::{self}` binds the last segment of the path.
        if let Some(last) = path.and_then(|p| last_segment(source, p)) {
            out.push(Binding {
                name: last,
                root: root.clone(),
                path: whole.clone(),
            });
        }
    }
}

/// Pushes a `Binding` for each name `node`, a node of a use tree, binds.
fn collect_bindings(source: &str, node: Node, prefix: Option<Prefix>, out: &mut Vec<Binding>) {
    match node.kind() {
        "identifier" | "self" | "crate" | "super" => {
            let text = node_text(source, node);
            out.push(Binding {
                root: prefix.map_or_else(|| text.clone(), |p| p.root.to_string()),
                path: joined(prefix, &text),
                name: text,
            });
        }
        "scoped_identifier" => {
            let Some(name) = node.child_by_field_name("name") else {
                return;
            };
            out.push(Binding {
                name: node_text(source, name),
                root: prefix.map_or_else(|| leftmost(source, node), |p| p.root.to_string()),
                path: joined(prefix, &node_text(source, node).replace(' ', "")),
            });
        }
        "scoped_use_list" => collect_list_bindings(source, node, prefix, out),
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_bindings(source, child, prefix, out);
            }
        }
        // `use_as_clause` binds its alias, `use_wildcard` binds nothing.
        _ => {}
    }
}

/// The first segment of a path node.
fn leftmost(source: &str, node: Node) -> String {
    let mut current = node;
    while let Some(path) = current
        .child_by_field_name("path")
        .filter(|_| current.kind() == "scoped_identifier")
    {
        current = path;
    }
    node_text(source, current)
}

/// The last segment of a path node.
fn last_segment(source: &str, node: Node) -> Option<String> {
    match node.kind() {
        "scoped_identifier" => node
            .child_by_field_name("name")
            .map(|n| node_text(source, n)),
        "identifier" | "crate" | "self" | "super" => Some(node_text(source, node)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{type_source, wildcards, Shown};
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
                type_source(src, function, "Set") == Shown::No,
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
            assert!(type_source(src, function, "Set") != Shown::No, "{src}");
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
            assert_eq!(type_source(src, function, "Set"), Shown::No, "{src}");
        }
    }

    /// What the file shows about `Set` for the only function of `src`.
    fn shown(src: &str) -> Shown {
        let tree = parse(src);
        let function = first_of_kind(tree.root_node(), "function_item").expect("a function");
        type_source(src, function, "Set")
    }

    fn import(root: &str) -> Shown {
        Shown::Import(root.to_string())
    }

    #[test]
    fn a_path_that_only_passes_through_the_name_does_not_bind_it() {
        for src in [
            "use Set::{A, B};\nfn make() -> Set { todo!() }",
            "use ext::Set::Variant;\nfn make() -> Set { todo!() }",
            "use ext::Set::{A, B};\nfn make() -> Set { todo!() }",
            "use ext::Set as Other;\nfn make() -> Set { todo!() }",
            "use ext::Other;\nfn make() -> Set { todo!() }",
        ] {
            assert_eq!(shown(src), Shown::No, "{src}");
        }
    }

    #[test]
    fn the_leaf_of_a_path_or_a_list_binds_the_name_with_the_first_segment_as_root() {
        for src in [
            "use ext::Set;\nfn make() -> Set { todo!() }",
            "pub use ext::Set;\nfn make() -> Set { todo!() }",
            "use ext::{Set, Other};\nfn make() -> Set { todo!() }",
            "use ext::{a::{Set}, Other};\nfn make() -> Set { todo!() }",
            "use ext::{a::b::Set, c};\nfn make() -> Set { todo!() }",
            "use ext::Set::{self, A};\nfn make() -> Set { todo!() }",
            "use ext::x::Set::{self};\nfn make() -> Set { todo!() }",
        ] {
            assert_eq!(shown(src), import("ext"), "{src}");
        }
    }

    #[test]
    fn a_path_of_the_same_crate_is_local_evidence_with_its_whole_path() {
        for (src, path) in [
            (
                "use crate::shapes::Set;\nfn make() -> Set { todo!() }",
                "crate::shapes::Set",
            ),
            (
                "use super::Set;\nfn make() -> Set { todo!() }",
                "super::Set",
            ),
            (
                "use self::inner::Set;\nfn make() -> Set { todo!() }",
                "self::inner::Set",
            ),
            (
                "use crate::Set;\nfn make() -> Set { todo!() }",
                "crate::Set",
            ),
            (
                "use crate::{shapes::Set, other};\nfn make() -> Set { todo!() }",
                "crate::shapes::Set",
            ),
            (
                "use crate::a::{b::{Set}, c};\nfn make() -> Set { todo!() }",
                "crate::a::b::Set",
            ),
            (
                "use crate::shapes::Set::{self};\nfn make() -> Set { todo!() }",
                "crate::shapes::Set",
            ),
        ] {
            assert_eq!(shown(src), Shown::LocalImport(path.to_string()), "{src}");
        }
    }

    #[test]
    fn a_definition_in_the_module_is_local_without_a_path() {
        let src = "struct Set;\nfn make() -> Set { todo!() }";
        assert_eq!(shown(src), Shown::Local);
    }

    #[test]
    fn two_paths_of_the_same_crate_for_one_name_show_nothing() {
        let src = "use crate::a::Set;\nuse crate::b::Set;\nfn make() -> Set { todo!() }";
        assert_eq!(shown(src), Shown::No);
    }

    #[test]
    fn two_imports_of_the_name_from_different_roots_show_nothing() {
        let src = "use a::Set;\nuse b::Set;\nfn make() -> Set { todo!() }";
        assert_eq!(shown(src), Shown::No);
        let src = "use a::Set;\nuse a::x::Set;\nfn make() -> Set { todo!() }";
        assert_eq!(shown(src), import("a"), "the same root twice is one root");
    }
}
