// parser::rust_item_gates: the `#[cfg]` facts about the Rust items of one file
// that a documentation claim checker needs and the graph does not keep.
//
// The graph records a gate only on a twin (issue #353): an ungated-looking
// `Function` row may still sit under `#[cfg(feature = "x")]`, and a `Variant`
// row says nothing about a `#[cfg]` on the variant. `doc_claims` may only call a
// count claim wrong when every declaration it counts is compiled by the build
// the claim is about, so it asks the source, once per file, through this module.
//
// What is read, per item:
// - its effective gate, the same `all` of every `#[cfg]` that reaches it the
//   twin ids use (`rust_cfg_gate::effective_gate`);
// - whether a `cfg_attr` that adds a `cfg` reaches it. `effective_gate` does
//   not expand `cfg_attr`, so such an item has no known gate. A `cfg_attr` that
//   adds anything else (`#![cfg_attr(not(test), no_std)]`) gates nothing;
// - for a function, whether it is nested in another function: the test harness
//   does not collect a `#[test]` on an inner item (rustc lint
//   `unnameable_test_items`), so a nested one is never run.
// source: The Rust Reference, "Conditional compilation" and "Testing attributes".

use tree_sitter::{Node, Parser};

use super::spec::rust_cfg_gate::effective_gate;
use super::{node_text, parse_tree_too_deep, parse_with_timeout, MAX_PARSE_BYTES, MAX_TREE_DEPTH};

/// A `function_item` of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionGate {
    /// 1-based line of the `fn` keyword's item (the graph's `start_line`).
    pub line: u64,
    /// Compact canonical gate, `""` when no `#[cfg]` reaches the item.
    pub gate: String,
    /// A `cfg_attr` adding a `cfg` reaches the item: its real gate is not known.
    pub cfg_attr: bool,
    /// The item is declared inside another function's body.
    pub nested: bool,
}

/// An `enum_item` of the file and its variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumShape {
    /// 1-based line of the item (the graph's `start_line`).
    pub line: u64,
    /// Variants declared in the body.
    pub variants: usize,
    /// Variants carrying a `#[cfg]` of their own, or a `cfg_attr` adding one:
    /// each may be absent from a build.
    pub gated_variants: usize,
}

/// Every `function_item` of `source`, in source order.
pub fn function_gates(source: &str) -> Result<Vec<FunctionGate>, String> {
    let tree = parse(source)?;
    let mut out = Vec::new();
    for node in items(tree.root_node(), "function_item") {
        out.push(FunctionGate {
            line: node.start_position().row as u64 + 1,
            gate: effective_gate(source, node),
            cfg_attr: reached_by_cfg_attr(source, node),
            nested: has_ancestor(node, "function_item"),
        });
    }
    Ok(out)
}

/// Every `enum_item` of `source`, in source order.
pub fn enum_shapes(source: &str) -> Result<Vec<EnumShape>, String> {
    let tree = parse(source)?;
    let mut out = Vec::new();
    for node in items(tree.root_node(), "enum_item") {
        let Some(body) = node.child_by_field_name("body") else {
            continue;
        };
        let mut cursor = body.walk();
        let variants: Vec<Node> = body
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "enum_variant")
            .collect();
        let gated_variants = variants
            .iter()
            .filter(|variant| own_attributes_name_cfg(source, **variant))
            .count();
        out.push(EnumShape {
            line: node.start_position().row as u64 + 1,
            variants: variants.len(),
            gated_variants,
        });
    }
    Ok(out)
}

/// Parses under the same size, time and depth bounds as `parse_file`.
fn parse(source: &str) -> Result<tree_sitter::Tree, String> {
    if source.len() as u64 > MAX_PARSE_BYTES {
        return Err(format!(
            "oversized: {} bytes > MAX_PARSE_BYTES {MAX_PARSE_BYTES}",
            source.len()
        ));
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|e| format!("rust grammar: {e}"))?;
    let tree = parse_with_timeout(&mut parser, source)?;
    if parse_tree_too_deep(tree.root_node(), MAX_TREE_DEPTH) {
        return Err(format!("parse tree deeper than {MAX_TREE_DEPTH}"));
    }
    Ok(tree)
}

/// The nodes of `kind` under `root`, in source order (explicit stack: the
/// depth was bounded by `parse`, but the walk still does not recurse).
fn items<'t>(root: Node<'t>, kind: &str) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            out.push(node);
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

fn has_ancestor(node: Node, kind: &str) -> bool {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if parent.kind() == kind {
            return true;
        }
        ancestor = parent.parent();
    }
    false
}

/// A `cfg_attr` adding a `cfg` among the outer attributes of `node` or of any
/// enclosing item, or among the inner attributes of an enclosing module body
/// or the file.
fn reached_by_cfg_attr(source: &str, node: Node) -> bool {
    let mut current = Some(node);
    while let Some(item) = current {
        if preceding_attributes(item)
            .into_iter()
            .any(|attr| cfg_attr_adds_cfg(source, attr))
        {
            return true;
        }
        if item.kind() == "declaration_list" || item.kind() == "source_file" {
            let mut cursor = item.walk();
            let inner_cfg_attr = item
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "inner_attribute_item")
                .any(|child| cfg_attr_adds_cfg(source, child));
            if inner_cfg_attr {
                return true;
            }
        }
        current = item.parent();
    }
    false
}

/// Whether an attribute item is `cfg_attr(pred, ..)` whose attributes include
/// a `cfg(..)`, directly or through a nested `cfg_attr`.
/// source: The Rust Reference, "The cfg_attr attribute" (`cfg_attr(pred,
/// attr, ..)` expands to the listed attributes when `pred` holds).
fn cfg_attr_adds_cfg(source: &str, attribute_item: Node) -> bool {
    if attribute_name(source, attribute_item).as_deref() != Some("cfg_attr") {
        return false;
    }
    let Some(arguments) = attribute_item
        .named_child(0)
        .and_then(|attribute| attribute.child_by_field_name("arguments"))
    else {
        return true; // unreadable: assume it may add one
    };
    arguments_add_cfg(&node_text(source, arguments))
}

/// `(pred, a, b(..), ..)`: whether one of the attributes after the predicate
/// is `cfg(..)`, or a `cfg_attr(..)` that adds one.
fn arguments_add_cfg(arguments: &str) -> bool {
    let inner = arguments
        .trim()
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(arguments);
    top_level_parts(inner).into_iter().skip(1).any(|part| {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("cfg_attr") {
            arguments_add_cfg(rest)
        } else {
            part.strip_prefix("cfg")
                .is_some_and(|rest| rest.trim_start().starts_with('('))
        }
    })
}

/// Splits on the commas outside any bracket or string literal.
fn top_level_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut in_string, mut escaped, mut start) = (0i32, false, false, 0);
    for (i, ch) in text.char_indices() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// The outer attributes of `item`, comments skipped, stopping at any other
/// sibling so an attribute never leaks to the next item.
fn preceding_attributes(item: Node) -> Vec<Node> {
    let mut found = Vec::new();
    let mut sibling = item.prev_named_sibling();
    while let Some(previous) = sibling {
        match previous.kind() {
            "attribute_item" => found.push(previous),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sibling = previous.prev_named_sibling();
    }
    found
}

/// The path of an `attribute_item`'s attribute (`cfg`, `cfg_attr`, `test`).
fn attribute_name(source: &str, attribute_item: Node) -> Option<String> {
    let attribute = attribute_item.named_child(0)?;
    if attribute.kind() != "attribute" {
        return None;
    }
    Some(node_text(source, attribute.named_child(0)?))
}

fn own_attributes_name_cfg(source: &str, item: Node) -> bool {
    preceding_attributes(item).into_iter().any(|attr| {
        attribute_name(source, attr).as_deref() == Some("cfg") || cfg_attr_adds_cfg(source, attr)
    })
}

#[cfg(test)]
#[path = "rust_item_gates_tests.rs"]
mod tests;
