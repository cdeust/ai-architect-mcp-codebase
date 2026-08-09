// parser::spec::structural_imports_calls — split out of `structural_imports.rs`
// purely for the coding-standards.md §4.1 500-line file cap once issue #224's
// Elixir/Zig/Bash follow-up landed (mirrors why `structural_scope.rs` and
// `structural_fallback_tests.rs` were split from their respective modules).
// SRP framing (§1.1): `structural_imports.rs` answers "which node is an
// import, via its own KEYWORD"; this module answers "which ALREADY-CLASSIFIED
// CALL is ALSO an import, via its NAME" — a genuinely separate mechanism
// (module doc item 5 there), not a a fragment of the same one. No new
// mechanism lives here beyond what `structural_imports.rs`'s own module doc
// already describes for item 5.

use tree_sitter::Node;

use super::structural_imports::{strip_quotes, text, BOUNDED_SEARCH_DEPTH};

/// Call names that mark an ALREADY-classified `CallSite` as also being an
/// import (Ruby's/Lua's `require`-family, plus issue #224's Elixir/Zig/Bash
/// follow-up). `"include"` is deliberately NOT in this table even though
/// Ruby has a `include SomeModule` mixin call, because it is a
/// module-inclusion mechanic, not a file/module IMPORT in this graph's
/// sense, and adding it was not verified necessary by any real failing case.
/// `"import"`/`"alias"`/`"use"` (Elixir — `"require"` was already present),
/// `"@import"` (Zig), and `"source"` (Bash) are each verified to reach
/// `CallSite` classification FIRST via a fix made in `structural.rs`/
/// `structural_fallback.rs` — three DIFFERENT classification paths (a
/// `target`-field call, a fieldless-call fallback, and a plain
/// `name`+`argument` call respectively) that all converge on the SAME
/// `CallEntry.callee` text this table matches against; this table only says
/// "and when the callee IS one of these names, also treat it as an import."
pub(super) const IMPORT_CALL_NAMES: [&str; 8] = [
    "require",
    "require_relative",
    "load",
    "import",
    "alias",
    "use",
    "@import",
    "source",
];

/// Field names probed for a classified call's argument list, checked in
/// order before falling back to a kind-based child scan. Mirrors
/// `structural.rs`'s `ARGUMENTS_FIELD_CANDIDATES` (Bash's singular
/// `argument`) — kept as its own local constant rather than shared across
/// the module boundary: this list is consumed exactly once, at this single
/// call site, and `structural.rs`'s copy is genuinely a different concern
/// (gates `call_callee_field`'s classification decision; this one only
/// locates an already-classified call's argument list).
const CALL_ARGUMENTS_FIELD_CANDIDATES: [&str; 2] = ["arguments", "argument"];

/// Ruby's/Lua's `require`-family, Elixir's `import`/`alias`/`require`/`use`,
/// Zig's `@import`, and Bash's `source` (module doc item 5, issue #224's
/// Elixir/Zig/Bash follow-up): given a `call`-shaped node already classified
/// as a `CallSite`, extracts the path. Two-step lookup, both global: (1) the
/// argument-list node itself — a `child_by_field_name` probe first (Ruby's
/// `argument_list` field, Lua's `arguments` field, Bash's singular
/// `argument` field, which for a single-argument command like `source
/// ./foo.sh` directly resolves to the argument NODE itself, not a wrapping
/// list), falling back to a kind-based child scan for grammars where the
/// argument list is a positional CHILD rather than a field at all (Elixir's
/// `call`, Zig's `builtin_function` — the same structural fact
/// `structural.rs`'s `target` branch and `structural_fallback.rs`'s
/// fieldless-call tier already rely on); (2) the path TEXT within that node
/// — a `"string"`-kind leaf scan first (Ruby/Lua's quoted string argument,
/// Zig's quoted `@import` argument), falling back to the first LEAF-shaped
/// named node (Elixir's `alias` node for `import Enum` is a bare,
/// non-string identifier-like leaf; Bash's single-argument case where the
/// argument field resolves directly to a leaf `word` node) — verified this
/// fallback never fires for Ruby/Lua/Zig's own string-literal case, since
/// the string-kind search always finds their quoted argument first.
pub(super) fn import_call_path(source: &str, call_node: Node) -> Option<String> {
    let args = CALL_ARGUMENTS_FIELD_CANDIDATES
        .iter()
        .find_map(|f| call_node.child_by_field_name(f))
        .or_else(|| find_named_child_of_kind(call_node, "arguments"))?;
    find_string_like_leaf(source, args, BOUNDED_SEARCH_DEPTH)
        .or_else(|| find_first_leaf_text(source, args, BOUNDED_SEARCH_DEPTH))
}

fn find_named_child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// Fallback for `import_call_path` step 2 when no `"string"`-kind leaf
/// exists in the argument node's subtree at all: the text of the first
/// LEAF-shaped named node found by pre-order DFS — a named node with no
/// named children of its own (Elixir's `alias`, a bare `word` field resolved
/// directly by the field probe above). Structurally identical recursion to
/// `find_string_like_leaf`, minus the kind filter; kept as a separate
/// function rather than a parameter because the two searches are tried in a
/// fixed, documented ORDER (string-shaped first), not as alternatives picked
/// by a caller.
fn find_first_leaf_text(source: &str, node: Node, depth: u8) -> Option<String> {
    if depth == 0 {
        return None;
    }
    let mut cursor = node.walk();
    let named: Vec<Node> = node.named_children(&mut cursor).collect();
    if named.is_empty() {
        let candidate = strip_quotes(&text(source, node));
        return if candidate.is_empty() {
            None
        } else {
            Some(candidate)
        };
    }
    for child in named {
        if let Some(found) = find_first_leaf_text(source, child, depth - 1) {
            return Some(found);
        }
    }
    None
}

fn find_string_like_leaf(source: &str, node: Node, depth: u8) -> Option<String> {
    if depth == 0 {
        return None;
    }
    if node.is_named() && node.kind().contains("string") {
        let candidate = strip_quotes(&text(source, node));
        if !candidate.is_empty() {
            return Some(candidate);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_string_like_leaf(source, child, depth - 1) {
            return Some(found);
        }
    }
    None
}
