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
