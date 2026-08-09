// parser::ast_capture — full-AST persistence capture.
//
// Independent of the symbol-extraction walkers (`spec::walk_defs` et al):
// those walk a SUBSET of the tree for named definitions/calls/imports and
// silently drop everything else (a raw expression, a punctuation token, a
// comment) because the target schema is purely semantic. This module walks
// EVERY tree-sitter node — named AND anonymous — and emits one row per node,
// so the store can answer "what is at this exact span" and reconstruct the
// whole tree without a second parse and without dropping any node class.
//
// Reuses the SAME grammar selection (`spec::grammar_for`) and the same
// timeout/depth guards (`parse_with_timeout`, `parse_tree_too_deep`,
// `MAX_TREE_DEPTH`) the symbol walkers use — no new per-language code, no new
// resource-bound policy.

use tree_sitter::{Parser, TreeCursor};

use super::spec::grammar_for;
use super::{
    count_parse_errors, parse_tree_too_deep, parse_with_timeout, Language, MAX_TREE_DEPTH,
};

/// One tree-sitter node, fully described.
///
/// # Contract
/// - `id` is stable and deterministic: `"{file_id}::ast::{preorder_index}"`,
///   where `preorder_index` is this node's rank in a pre-order DFS starting
///   from the tree root (root = 0). Same source text -> same ids.
/// - `parent_id` is `""` only for the tree root; every other row's
///   `parent_id` names an `id` that appears earlier in the same capture.
/// - `child_index` is this node's 0-based position among ALL of its parent's
///   children, named and anonymous, in the same order `TreeCursor`/
///   `Node::child(i)` iterate (a tree-sitter API guarantee — both traverse
///   the identical child list). Replaying rows ordered by
///   `(parent_id, child_index)` reconstructs the tree exactly.
/// - `field_name` is the field this node occupies in its parent (`""` when
///   the grammar assigns none — most anonymous tokens, and any named child
///   the grammar doesn't field-name).
pub struct AstNodeRow {
    pub id: String,
    pub kind: String,
    pub is_named: bool,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u64,
    pub start_col: u64,
    pub end_line: u64,
    pub end_col: u64,
    pub field_name: String,
    pub child_index: u64,
    pub parent_id: String,
}

pub struct AstCapture {
    /// Pre-order: `nodes[0]` is always the tree root.
    pub nodes: Vec<AstNodeRow>,
    /// Mirrors `ParseResult::parse_errors` — count of tree-sitter ERROR/
    /// MISSING nodes in this SAME tree (computed once here; capturing is
    /// already a full walk, so this is a second pass over the same nodes,
    /// not a second parse).
    pub parse_errors: u32,
}

/// Parses `source` and captures every tree-sitter node.
///
/// # Contract
/// - **pre:** `file_id` is the file's repo-relative id (used only as the id
///   prefix — this function does no I/O); `lang` names a language
///   `spec::grammar_for` resolves (all eleven supported languages do).
/// - **post:** `Ok(AstCapture)` whose `nodes` is the COMPLETE tree in
///   pre-order (length == the tree's total node count, named + anonymous),
///   or a clean `Err` on the same two conditions `parser::parse_file` rejects
///   on: grammar-set failure or parse timeout/rejection. Never panics on
///   malformed input — tree-sitter recovers into ERROR/MISSING nodes and
///   this walks those like any other node.
/// - **invariant:** bounded independently of how adversarial `source` is, by
///   the same `MAX_TREE_DEPTH` guard `parse_with_spec` applies (checked
///   before the walk starts, so a rejected tree costs zero walk time).
pub fn capture_full_ast(source: &str, file_id: &str, lang: Language) -> Result<AstCapture, String> {
    let ts_lang = grammar_for(lang, file_id)
        .ok_or_else(|| format!("ast_capture: no grammar for language {lang:?}"))?;
    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .map_err(|e| format!("ast_capture: failed to set {lang:?} language: {e}"))?;
    let tree = parse_with_timeout(&mut parser, source)?;
    let root = tree.root_node();
    if parse_tree_too_deep(root, MAX_TREE_DEPTH) {
        return Err(format!(
            "ast_capture: parse_tree_too_deep: exceeds {MAX_TREE_DEPTH} levels"
        ));
    }
    let parse_errors = count_parse_errors(root);

    let mut nodes = Vec::new();
    let mut counter: u64 = 0;
    let mut cursor: TreeCursor = root.walk();
    walk(file_id, &mut cursor, "", 0, &mut counter, &mut nodes);
    Ok(AstCapture {
        nodes,
        parse_errors,
    })
}

/// Walks `cursor`'s current node and every one of its siblings (in the order
/// the cursor iterates — named and anonymous alike), recursing into each
/// child's own subtree first. Bounded by the `MAX_TREE_DEPTH` check the
/// caller already ran on the whole tree before this starts; each recursive
/// call here descends exactly one tree level, mirroring the recursion shape
/// the (already-bounded) symbol walkers use for the same reason.
fn walk(
    file_id: &str,
    cursor: &mut TreeCursor,
    parent_id: &str,
    start_child_index: u64,
    counter: &mut u64,
    out: &mut Vec<AstNodeRow>,
) {
    let mut child_index = start_child_index;
    loop {
        let node = cursor.node();
        let id = format!("{file_id}::ast::{counter}");
        *counter += 1;
        let start = node.start_position();
        let end = node.end_position();
        out.push(AstNodeRow {
            id: id.clone(),
            kind: node.kind().to_string(),
            is_named: node.is_named(),
            start_byte: node.start_byte() as u64,
            end_byte: node.end_byte() as u64,
            // 1-based lines, matching the rest of this crate's convention
            // (`count_parse_errors`/`collect_error_ranges`/`ExtractedNode`).
            start_line: start.row as u64 + 1,
            start_col: start.column as u64,
            end_line: end.row as u64 + 1,
            end_col: end.column as u64,
            field_name: cursor.field_name().unwrap_or("").to_string(),
            child_index,
            parent_id: parent_id.to_string(),
        });
        if cursor.goto_first_child() {
            walk(file_id, cursor, &id, 0, counter, out);
            cursor.goto_parent();
        }
        child_index += 1;
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_has_no_parent_and_index_zero() {
        let cap = capture_full_ast("fn main() {}\n", "f.rs", Language::Rust).unwrap();
        assert!(!cap.nodes.is_empty());
        let root = &cap.nodes[0];
        assert_eq!(root.parent_id, "");
        assert_eq!(root.child_index, 0);
        assert_eq!(root.id, "f.rs::ast::0");
    }

    #[test]
    fn ids_are_deterministic_across_identical_parses() {
        let src = "fn main() { let x = 1; }\n";
        let a = capture_full_ast(src, "f.rs", Language::Rust).unwrap();
        let b = capture_full_ast(src, "f.rs", Language::Rust).unwrap();
        let a_ids: Vec<&str> = a.nodes.iter().map(|n| n.id.as_str()).collect();
        let b_ids: Vec<&str> = b.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(a_ids, b_ids);
    }

    #[test]
    fn every_non_root_parent_id_names_an_earlier_row() {
        let cap = capture_full_ast(
            "fn main() { let x = 1; println!(\"{}\", x); }\n",
            "f.rs",
            Language::Rust,
        )
        .unwrap();
        let mut seen = std::collections::HashSet::new();
        for row in &cap.nodes {
            if !row.parent_id.is_empty() {
                assert!(
                    seen.contains(&row.parent_id),
                    "row {} references parent {} before it was emitted",
                    row.id,
                    row.parent_id
                );
            }
            seen.insert(row.id.clone());
        }
    }

    #[test]
    fn captures_anonymous_nodes_too() {
        // A Rust fn's `{`/`}`/`(`/`)` are anonymous tokens — if the walker
        // only visited named nodes, this file's capture would be missing
        // most of its rows (full-AST persistence's whole point).
        let cap = capture_full_ast("fn main() {}\n", "f.rs", Language::Rust).unwrap();
        assert!(
            cap.nodes.iter().any(|n| !n.is_named),
            "capture must include anonymous (unnamed) nodes, not just named ones"
        );
    }

    #[test]
    fn ruby_resolves_via_the_shallow_spec_fallback() {
        // Ruby has no deep LangSpec (ADR-0056) — this exercises the one
        // fallback branch in `grammar_for`.
        let cap = capture_full_ast("def f\n  1\nend\n", "f.rb", Language::Ruby).unwrap();
        assert!(!cap.nodes.is_empty());
    }
}
