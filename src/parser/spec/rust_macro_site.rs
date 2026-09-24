// parser::spec::rust_macro_site — what the parser records on the CallSite of a
// macro invocation so the resolver can pick the expansion's target (issue #339).
//
// A macro whose expansion depends on its arguments cannot be resolved from its
// name alone:
//   - `write!(dst, ..)` / `writeln!(dst, ..)` expand to `dst.write_fmt(..)`, a
//     method of `fmt::Write`, of `io::Write`, or inherent to `fmt::Formatter`,
//     chosen by the type of `dst`;
//   - `vec![]`, `vec![x; n]` and `vec![a, b]` expand to three different calls.
// The parser sees both facts, so it records them: `receiver_hint` (the declared
// type of a simple local destination, the same value `rust_receiver` computes
// for a method receiver) and `macro_arg_shape` (`empty`, `repeat` or `list` for
// `vec!`; `local` or `expr` for the destination of `write!`).
//
// source: https://doc.rust-lang.org/std/macro.write.html and
// https://doc.rust-lang.org/std/macro.vec.html (expansions);
// tree-sitter-rust 0.24.2 `token_tree` children (delimiters and `,` / `;` are
// anonymous children, nested groups are `token_tree` children).

use tree_sitter::Node;

use super::conventions::CallEntry;
use super::rust::{RustConventions, RUST_FAMILY};
use crate::parser::node_text;

use crate::macro_expansion::rust::{DEST_MACROS, VEC_MACROS};

const SHAPE_PROPERTY: &str = "macro_arg_shape";

// The shape names `macro_expansion::dispatch` keys on.
const SHAPE_EMPTY: &str = "empty";
const SHAPE_REPEAT: &str = "repeat";
const SHAPE_LIST: &str = "list";
// A `write!` destination that is a plain local, or anything else.
const SHAPE_LOCAL: &str = "local";
const SHAPE_EXPR: &str = "expr";

impl RustConventions {
    /// The `CallSite` of one macro invocation, with the macro facts the
    /// resolver reads. `callee` is the macro path with its `!`.
    pub(super) fn macro_call_site(
        callee: &str,
        call_node: Node,
        caller_qn: &str,
        source: &str,
    ) -> CallEntry {
        let name = macro_base_name(callee);
        let token_tree = call_node
            .children(&mut call_node.walk())
            .find(|c| c.kind() == RUST_FAMILY.token_tree_kind);
        let dest = token_tree
            .filter(|_| DEST_MACROS.contains(&name))
            .map(|tt| plain_local_destination(source, tt));
        // The hint of a macro site carries the type as written (`fmt::Formatter`):
        // the macro pass resolves it in the scope of the file (issue #339).
        let hint = dest
            .flatten()
            .and_then(|d| super::rust_receiver::receiver_type_path(source, d));
        let mut entry = Self::call_site_spanning(
            callee,
            call_node,
            call_node.end_byte() as u64,
            caller_qn,
            hint,
        );
        let shape = match (dest, token_tree) {
            (Some(Some(_)), _) => Some(SHAPE_LOCAL),
            (Some(None), _) => Some(SHAPE_EXPR),
            (None, Some(tt)) if VEC_MACROS.contains(&name) => Some(vec_shape(tt)),
            _ => None,
        };
        if let Some(shape) = shape {
            entry
                .properties
                .push((SHAPE_PROPERTY.to_string(), shape.to_string()));
        }
        entry
    }
}

/// `std::write!` and `write!` are the same macro: the last path segment,
/// without the `!`.
fn macro_base_name(callee: &str) -> &str {
    let bare = callee.strip_suffix('!').unwrap_or(callee);
    bare.rsplit("::").next().unwrap_or(bare)
}

/// The first macro argument when it is a plain local (`f`, `&mut f`); `None`
/// for a field, a call or any other expression, whose type no `let` or
/// parameter declares.
fn plain_local_destination<'t>(source: &str, token_tree: Node<'t>) -> Option<Node<'t>> {
    let mut cursor = token_tree.walk();
    let mut destination: Option<Node> = None;
    for child in token_tree.children(&mut cursor).skip(1) {
        match child.kind() {
            "," => break,
            "identifier" if destination.is_none() => destination = Some(child),
            _ if matches!(node_text(source, child).as_str(), "&" | "mut") => {}
            _ => return None,
        }
    }
    destination
}

/// `empty`, `repeat` or `list`, read from the macro's own delimiters.
fn vec_shape(token_tree: Node) -> &'static str {
    let mut cursor = token_tree.walk();
    let inner: Vec<Node> = token_tree
        .children(&mut cursor)
        .filter(|c| c.is_named() || c.kind() == ";" || c.kind() == ",")
        .collect();
    if inner.is_empty() {
        SHAPE_EMPTY
    } else if inner.iter().any(|c| c.kind() == ";") {
        SHAPE_REPEAT
    } else {
        SHAPE_LIST
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::{parse_file, Language};

    fn sites(src: &str) -> Vec<(String, Vec<(String, String)>)> {
        let parsed = parse_file(src, "src/lib.rs", Language::Rust).expect("parse");
        parsed
            .nodes
            .iter()
            .filter(|n| n.label == "CallSite")
            .map(|n| (n.name.clone(), n.properties.clone()))
            .collect()
    }

    fn prop<'a>(props: &'a [(String, String)], key: &str) -> Option<&'a str> {
        props
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_formatter_destination_is_recorded_as_the_receiver_hint() {
        let src = "use std::fmt;\nfn show(f: &mut fmt::Formatter<'_>) -> fmt::Result {\n    write!(f, \"x\")\n}\n";
        let all = sites(src);
        let (_, props) = all.iter().find(|(n, _)| n == "write!").expect("write!");
        assert_eq!(prop(props, "receiver_hint"), Some("fmt::Formatter"));
    }

    #[test]
    fn a_mut_reference_destination_keeps_its_hint() {
        let src = "use std::io::BufWriter;\nfn out(mut w: BufWriter<Vec<u8>>) {\n    writeln!(&mut w, \"x\").ok();\n}\n";
        let all = sites(src);
        let (_, props) = all.iter().find(|(n, _)| n == "writeln!").expect("writeln!");
        assert_eq!(prop(props, "receiver_hint"), Some("BufWriter"));
    }

    #[test]
    fn a_field_destination_has_no_hint() {
        let src = "struct S { w: Vec<u8> }\nimpl S {\n    fn go(&mut self) {\n        write!(self.w, \"x\").ok();\n    }\n}\n";
        let all = sites(src);
        let (_, props) = all.iter().find(|(n, _)| n == "write!").expect("write!");
        assert_eq!(prop(props, "receiver_hint"), None);
    }

    #[test]
    fn a_plain_local_destination_and_an_expression_destination_are_told_apart() {
        let src = "struct S { w: Vec<u8> }\nimpl S {\n    fn go(&mut self, mut o: Vec<u8>) {\n        write!(o, \"a\").ok();\n        write!(self.w, \"b\").ok();\n    }\n}\n";
        let shapes: Vec<String> = sites(src)
            .iter()
            .filter(|(n, _)| n == "write!")
            .filter_map(|(_, p)| prop(p, "macro_arg_shape").map(str::to_string))
            .collect();
        let mut sorted = shapes.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["expr", "local"], "{shapes:?}");
    }

    #[test]
    fn vec_argument_shapes_are_recorded() {
        let src = "fn a(n: usize) {\n    let _e: Vec<u8> = vec![];\n    let _r = vec![0u8; n];\n    let _l = vec![1, 2, 3];\n}\n";
        let mut shapes: Vec<String> = sites(src)
            .iter()
            .filter(|(n, _)| n == "vec!")
            .filter_map(|(_, p)| prop(p, "macro_arg_shape").map(str::to_string))
            .collect();
        shapes.sort();
        assert_eq!(shapes, vec!["empty", "list", "repeat"]);
    }
}
