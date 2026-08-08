// parser::spec::structural_scope — node-level text/field helpers and the
// definition-scope resolver shared by `structural.rs`'s classify/emit
// pipeline. Split out purely for coding-standards.md §4.1 (structural.rs was
// pushing past the 500-line file cap once the TIER 2 wiring landed) — SRP
// framing: this module answers "given already-classified definitions, what
// are their scopes and qualified names?"; `structural.rs` answers "which
// nodes ARE definitions?". No new mechanism lives here.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::parser::{node_text, qual};

const QN_SEP: &str = "::";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DefRole {
    FunctionLike,
    TypeLike,
}

pub(super) struct DefEntry<'t> {
    pub node: Node<'t>,
    pub role: DefRole,
    pub name: String,
    pub is_method: bool,
    pub qn: String,
    /// Set via `visibility_via_modifier_child` (TIER 2 item 4) — a
    /// node-KIND heuristic, not field-based, since no grammar in the
    /// verified sample exposes visibility as a field (see `structural.rs`'s
    /// module doc).
    pub visibility: String,
}

pub(super) struct CallEntry<'t> {
    pub node: Node<'t>,
    pub callee: String,
}

pub(super) fn has_field(node: Node, field: &str) -> bool {
    node.child_by_field_name(field).is_some()
}

pub(super) fn any_field(node: Node, fields: &[&str]) -> bool {
    fields.iter().any(|f| has_field(node, f))
}

pub(super) fn line_of(node: Node) -> u64 {
    node.start_position().row as u64 + 1
}

pub(super) fn end_line_of(node: Node) -> u64 {
    node.end_position().row as u64 + 1
}

/// Reduces a possibly-qualified callee/heritage name to its trailing segment
/// (`Utils.parse` -> `parse`, `a::b::c` -> `c`). Duplicated from
/// `shallow::last_segment` rather than shared: the current call sites in
/// this module plus shallow's own don't cross the `coding-standards.md`
/// §3.3 three-use threshold for extraction across module boundaries the
/// same way — kept local so this module stays readable standalone.
pub(super) fn last_segment(text: &str) -> String {
    const SEPARATORS: [&str; 3] = [".", QN_SEP, "->"];
    let mut tail = text.trim();
    for sep in SEPARATORS {
        if let Some(idx) = tail.rfind(sep) {
            let candidate = &tail[idx + sep.len()..];
            if !candidate.trim().is_empty() {
                tail = candidate.trim();
            }
        }
    }
    tail.to_string()
}

/// The text of the rightmost named leaf under `node` (or `node` itself if it
/// has no named children). Tree-sitter marks a grammar's keywords as
/// UNNAMED children (`extends`, `implements`) and its content (identifiers,
/// type references) as NAMED ones — the same convention `shallow.rs`'s
/// `named_children` already relies on. A heritage field's target node is
/// often a wrapper spanning the introducing keyword plus the actual type
/// reference (verified: Java's `superclass` field node's text is literally
/// `"extends Base"`), so reading the WHOLE node's text would leak the
/// keyword into the edge target; descending to the rightmost named leaf
/// strips it structurally, with no per-language keyword list.
pub(super) fn rightmost_named_leaf_text(source: &str, node: Node) -> String {
    let mut current = node;
    loop {
        let mut cursor = current.walk();
        let named: Vec<Node> = current.named_children(&mut cursor).collect();
        match named.last() {
            Some(&last) => current = last,
            None => break,
        }
    }
    node_text(source, current)
}

/// Finds the nearest ancestor of `node` already present in `node_id_to_def`,
/// returning its index. `None` means file scope.
pub(super) fn enclosing_def_index(
    node: Node,
    node_id_to_def: &HashMap<usize, usize>,
) -> Option<usize> {
    let mut current = node.parent();
    while let Some(n) = current {
        if let Some(&idx) = node_id_to_def.get(&n.id()) {
            return Some(idx);
        }
        current = n.parent();
    }
    None
}

/// Computes each definition's qualified name and method/function split,
/// mirroring `shallow.rs`'s dedup discipline (a name collision at the same
/// scope gets a `@{start_line}` suffix).
///
/// Preconditions: `defs` is fully populated and sorted by AST pre-order
/// (`node.start_byte()`) so an ancestor's qn is always resolved before a
/// descendant needs it. Postconditions: every `DefEntry.qn` is set and
/// `is_method` reflects either an own `receiver` field or a type-like
/// enclosing definition.
pub(super) fn resolve_scopes(defs: &mut [DefEntry], file_path: &str) {
    let node_id_to_def: HashMap<usize, usize> = defs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.node.id(), i))
        .collect();

    let mut emitted_qns: HashSet<String> = HashSet::new();
    for i in 0..defs.len() {
        let node = defs[i].node;
        let name = defs[i].name.clone();
        let start_line = line_of(node);
        let parent_idx = enclosing_def_index(node, &node_id_to_def);
        let scope = match parent_idx {
            Some(idx) => defs[idx].qn.clone(),
            None => file_path.to_string(),
        };
        let parent_is_type_like = parent_idx.is_some_and(|idx| defs[idx].role == DefRole::TypeLike);
        if defs[i].role == DefRole::FunctionLike {
            defs[i].is_method = has_field(node, "receiver") || parent_is_type_like;
        }
        let candidate = qual(&scope, &name);
        let qn = if emitted_qns.insert(candidate.clone()) {
            candidate
        } else {
            let unique = format!("{candidate}@{start_line}");
            emitted_qns.insert(unique.clone());
            unique
        };
        defs[i].qn = qn;
    }
}
