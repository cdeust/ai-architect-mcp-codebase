// parser::spec::walkers::embedded — the embedded-language re-parse walker.
//
// Split out of `walkers/mod.rs` (issue #101, §4.1) as a pure move; the
// `ExtractedNode`/`ExtractedRef` contract is unchanged.

use tree_sitter::{Node, Parser};

use super::super::lang_spec::LangSpec;
use super::{walk_defs, WalkCtx};
use crate::parser::{node_text, parse_with_timeout};

/// Embedded-language walker: for each embedded rule, locate every script node
/// under `parent`, re-parse its content child with the embedded language's
/// grammar, and run the generic def walker on the inner tree under `scope`.
/// Empty `spec.embedded` (all ten core languages) makes this a no-op.
pub(super) fn walk_embedded(spec: &LangSpec, ctx: &mut WalkCtx, parent: Node, scope: &str) {
    if spec.embedded.is_empty() {
        return;
    }
    for emb in spec.embedded {
        let inner_spec = match super::super::registry::lang_spec(emb.embedded_language) {
            Some(s) => s,
            None => continue,
        };
        let mut stack = vec![parent];
        while let Some(n) = stack.pop() {
            if n.kind() == emb.script_node_kind {
                if let Some(content) = find_child_of_kind(n, emb.content_node_kind) {
                    reparse_embedded(inner_spec, ctx, content, scope);
                }
            }
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                stack.push(c);
            }
        }
    }
}

fn find_child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// Re-parses the byte slice of `content` with `inner_spec`'s grammar and walks
/// the inner tree. The inner nodes/refs are appended to the same `WalkCtx`, so
/// embedded symbols land in the host file's `ParseResult` under `scope`.
fn reparse_embedded(inner_spec: &LangSpec, ctx: &mut WalkCtx, content: Node, scope: &str) {
    let text = node_text(ctx.source, content);
    let lang: tree_sitter::Language = (inner_spec.ts_language)();
    let mut parser = Parser::new();
    if parser.set_language(&lang).is_err() {
        return;
    }
    let tree = match parse_with_timeout(&mut parser, &text) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Inner walk runs on a scoped sub-context so the embedded source's byte
    // offsets resolve against `text`, then its output is merged into `ctx`.
    let mut inner = WalkCtx {
        source: &text,
        // The embedded walk keeps the HOST file's id: an embedded region is part
        // of the host file, so any file-scoped shaping (the Rust walker's `impl`
        // receiver) must still resolve to the host path.
        file_path: ctx.file_path,
        nodes: Vec::new(),
        refs: Vec::new(),
        next_seq: ctx.next_seq,
        emitted_qns: std::mem::take(&mut ctx.emitted_qns),
    };
    walk_defs(inner_spec, &mut inner, tree.root_node(), scope, None);
    ctx.next_seq = inner.next_seq;
    ctx.emitted_qns = inner.emitted_qns;
    ctx.nodes.append(&mut inner.nodes);
    ctx.refs.append(&mut inner.refs);
}
