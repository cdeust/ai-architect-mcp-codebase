// parser::spec::rust_macro_binds: the names a macro invocation may bind, which
// no pattern node shows because a token tree is not parsed (issues #348, #349).
// Read by `rust_scope`, so that a name a macro may rebind is never taken as
// bound exactly once.

use std::collections::HashSet;

use tree_sitter::Node;

use crate::parser::node_text;

// source: std macros whose arguments are only used, never bound, as measured
// on the expansions of `assert_eq!`, `println!`, `format!` and their siblings
// (https://doc.rust-lang.org/std/#macros). Any other macro, a `macro_rules!`
// of the crate for one, may bind a name it is handed (`bind!(s, o)` expanding
// to `let $n = $e;`), and a token tree is not parsed, so no pattern node shows it.
const NON_BINDING_MACROS: [&str; 19] = [
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "print",
    "println",
    "eprint",
    "eprintln",
    "format",
    "format_args",
    "write",
    "writeln",
    "panic",
    "todo",
    "unimplemented",
    "unreachable",
    "dbg",
];

/// The identifiers of a token tree, and whether it holds a token able to
/// introduce a binding (`let`, `for`, `match`, `if`, `|`, `=>`). String
/// literals are skipped: a word in a message is not a name.
fn token_tree_words(source: &str, tree: Node) -> (Vec<String>, bool) {
    let mut words = Vec::new();
    let mut binds = false;
    let mut stack = vec![tree];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "string_literal" | "raw_string_literal" | "char_literal" => continue,
            "identifier" => words.push(node_text(source, node)),
            "let" | "for" | "match" | "if" | "|" | "=>" => binds = true,
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    (words, binds)
}

/// Names a macro invocation may bind, which no pattern node shows because a
/// token tree is not parsed: every identifier of the token tree of a macro
/// that is not in `NON_BINDING_MACROS`, and, for one that is, of a token tree
/// that holds a token able to introduce a binding (`let`, `for`, `match`,
/// `|`, `=>`, `if`). `assert_eq!(s.m(), ..)` only uses `s` and keeps it.
pub(super) fn names_macros_may_rebind(source: &str, scope: Node) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if node.kind() == "macro_invocation" {
            let known = node
                .child_by_field_name("macro")
                .map(|m| node_text(source, m))
                .and_then(|m| m.rsplit("::").next().map(str::to_string))
                .is_some_and(|m| NON_BINDING_MACROS.contains(&m.as_str()));
            let mut cursor = node.walk();
            let tree = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "token_tree");
            if let Some(tree) = tree {
                let (words, binds) = token_tree_words(source, tree);
                if !known || binds {
                    names.extend(words);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::token_tree_words;
    use tree_sitter::{Node, Parser};

    fn first_token_tree<'t>(node: Node<'t>) -> Option<Node<'t>> {
        if node.kind() == "token_tree" {
            return Some(node);
        }
        let mut cursor = node.walk();
        let found = node.children(&mut cursor).find_map(first_token_tree);
        found
    }

    /// `(words, binds)` of the first token tree of `src`.
    fn read(src: &str) -> (Vec<String>, bool) {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("language");
        let tree = parser.parse(src, None).expect("parse");
        let tt = first_token_tree(tree.root_node()).expect("a token tree");
        token_tree_words(src, tt)
    }

    // source: measured on tree-sitter-rust 0.24.2 (Cargo.lock): the keywords
    // `let`, `for`, `match`, `if` and the punctuation `|` and `=>` are children
    // of a `token_tree`, so `token_tree_words` sees them. If a grammar bump
    // stopped emitting one, the matching test fails and the rule must be
    // revisited: the name would then be taken as never rebound.
    #[test]
    fn each_binder_token_is_a_child_of_the_token_tree() {
        for (src, what) in [
            ("fn f() { m!(let s = 1); }", "let"),
            ("fn f() { m!(for s in v {}); }", "for"),
            ("fn f() { m!(match s { _ => 1 }); }", "match"),
            ("fn f() { m!(if s {}); }", "if"),
            ("fn f() { m!(v.iter().all(|s| s.m())); }", "|"),
            ("fn f() { m!(s => 1); }", "=>"),
        ] {
            assert!(read(src).1, "no binder token seen for `{what}` in {src}");
        }
    }

    #[test]
    fn a_plain_use_of_the_name_has_no_binder_token_and_a_string_is_skipped() {
        let (words, binds) = read("fn f() { m!(s.m(), \"let s in a | b => c\"); }");
        assert!(!binds);
        assert!(words.contains(&"s".to_string()));
        assert!(!words.contains(&"let".to_string()));
    }
}
