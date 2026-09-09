// parser::spec::rust_scope — which names are BOUND at a call site, so the
// speculative by-value argument scan (issue #87) can tell a value from a
// function reference.
// source: ADR-9836, measured on 2026-09-09 against DYResearch/dy-wcet @ 1e93ccd.

use std::collections::HashSet;

use tree_sitter::Node;

use crate::parser::node_text;

/// Node kinds that open a new binding scope for this analysis.
/// source: tree-sitter-rust 0.24.2 src/node-types.json.
const SCOPE_KINDS: [&str; 2] = ["function_item", "closure_expression"];

/// Node kinds that introduce bindings through a `pattern` field.
/// source: tree-sitter-rust 0.24.2 src/node-types.json (`parameter` and
/// `let_declaration` both declare a required `pattern` field).
const BINDING_KINDS: [&str; 2] = ["parameter", "let_declaration"];

/// The `pattern` field name shared by both binding kinds.
const PATTERN_FIELD: &str = "pattern";

/// The tree-sitter-rust leaf kind for a bare name, matching the constant the
/// macro scan already relies on.
const IDENTIFIER_KIND: &str = "identifier";

/// Every name bound by the function or closure enclosing `call_node`: its
/// parameters and its `let` declarations.
///
/// precondition: `call_node` is a node inside a parsed Rust tree.
/// postcondition: the returned set contains only identifier texts read from
/// `source`; an empty set when `call_node` sits outside any function.
/// Scope is approximated at function granularity by design; see ADR-9836.
pub(super) fn bound_names_in_scope(source: &str, call_node: Node) -> HashSet<String> {
    let mut names = HashSet::new();
    let Some(scope) = enclosing_scope(call_node) else {
        return names;
    };
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if BINDING_KINDS.contains(&node.kind()) {
            if let Some(pattern) = node.child_by_field_name(PATTERN_FIELD) {
                collect_identifiers(source, pattern, &mut names);
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    names
}

/// The nearest ancestor that opens a binding scope, `call_node` itself
/// included.
fn enclosing_scope(call_node: Node) -> Option<Node> {
    let mut node = call_node;
    loop {
        if SCOPE_KINDS.contains(&node.kind()) {
            return Some(node);
        }
        node = node.parent()?;
    }
}

/// Harvests every `identifier` leaf inside one pattern, so destructuring
/// binds all of its names rather than only the simple case.
fn collect_identifiers(source: &str, pattern: Node, out: &mut HashSet<String>) {
    let mut stack = vec![pattern];
    while let Some(node) = stack.pop() {
        if node.kind() == IDENTIFIER_KIND {
            let text = node_text(source, node);
            if !text.is_empty() {
                out.insert(text);
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}
