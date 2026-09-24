// parser::spec::rust_item_binds: names a function's own items bind, and whether
// a `let` reaches a call (issues #348, #349).
//
// A receiver hint needs its name bound exactly once. Besides patterns, three
// kinds of item bind a name a call may denote: `const` and `static` items, a
// const generic parameter, and a `use`. And a `let` binds only inside its own
// block and only after it: `{ let s = make(); } s.m()` and `s.m(); let s = ..`
// name something else. This is the minimal safe cut, not a scope analysis:
// where a fact is not shown, the name is declined.

use std::collections::HashSet;

use tree_sitter::Node;

use crate::parser::node_text;

/// source: tree-sitter-rust 0.24.2 src/node-types.json (`const_item`,
/// `static_item` and `const_parameter` declare a `name` field).
const NAMED_ITEM_KINDS: [&str; 3] = ["const_item", "static_item", "const_parameter"];

/// Every name a `const`, `static`, const generic or `use` inside `scope` binds.
/// A `use` contributes every identifier of its path, which only over-declines.
pub(super) fn names_items_bind(source: &str, scope: Node) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if NAMED_ITEM_KINDS.contains(&node.kind()) {
            if let Some(name) = node.child_by_field_name("name") {
                names.insert(node_text(source, name));
            }
        } else if node.kind() == "use_declaration" {
            let text = node_text(source, node);
            names.extend(
                text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .filter(|w| !w.is_empty())
                    .map(str::to_string),
            );
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    names
}

/// True when the `let` at `declaration` binds at `call`: its enclosing block
/// contains the call and the declaration ends before the call starts. A
/// declaration that is not a `let` in a block is declined.
pub(super) fn declaration_reaches(declaration: Node, call: Node) -> bool {
    let Some(block) = declaration.parent().filter(|p| p.kind() == "block") else {
        return false;
    };
    is_ancestor(block, call) && declaration.end_byte() <= call.start_byte()
}

pub(super) fn is_ancestor(ancestor: Node, node: Node) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.id() == ancestor.id() {
            return true;
        }
        current = n.parent();
    }
    false
}
