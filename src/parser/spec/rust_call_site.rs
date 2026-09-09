// parser::spec::rust_call_site — Rust `CallSite`/`CallEntry` shaping,
// including the issue #283 palier 3 `receiver_hint` plumbing.
//
// Split out of `rust.rs` (ADR-0055 phase 8's `RustConventions`) to keep that
// file under the §4.1 500-line cap — a second `impl RustConventions` block in
// a sibling module, same pattern `rust_scope`/`rust_receiver`/
// `rust_macro_calls` already use for their own `super::rust::...` calls back
// into this struct's methods.
//
// source: tasks/plan-issues-282-283-284.md §2.3 (lot 6, issue #283 palier 3).

use tree_sitter::Node;

use super::conventions::CallEntry;
use super::rust::RustConventions;

impl RustConventions {
    /// Shapes one `CallSite` keyed on `span_node`'s source span. Chained calls
    /// share a start byte (`input.trim().to_string()`), so the (start, end) byte
    /// span — not the start alone — is what makes the id unique among a caller's
    /// call sites. The column is 0-based, as the pre-migration walker emitted it.
    ///
    /// `source` computes the issue #283 palier 3 `receiver_hint`
    /// (`rust_receiver::receiver_hint`) from `span_node`'s own shape — this
    /// path is only for a real `call_expression`, where `span_node` IS the
    /// call node `receiver_hint` inspects. Callers with no meaningful
    /// receiver to derive a hint from (issue #87's by-value function-argument
    /// sites, never a method receiver) go through `call_site_spanning`
    /// directly with an explicit `None` instead of this helper.
    pub(super) fn call_site(
        callee: &str,
        span_node: Node,
        caller_qn: &str,
        source: &str,
    ) -> CallEntry {
        Self::call_site_spanning(
            callee,
            span_node,
            span_node.end_byte() as u64,
            caller_qn,
            super::rust_receiver::receiver_hint(source, span_node),
        )
    }

    /// Same shape as `call_site`, but the span's end byte and the
    /// already-computed `receiver_hint` are supplied separately from the
    /// start node. Needed by the macro-argument scan (`rust_macro_calls`): a
    /// reconstructed callee like `s.slack_of` spans two sibling `identifier`
    /// nodes with an anonymous `.`/`::` token between them, so no single node
    /// covers the whole span the way a `call_expression` node does, and its
    /// receiver hint is derived from the isolated receiver node directly
    /// rather than from `start_node`.
    pub(super) fn call_site_spanning(
        callee: &str,
        start_node: Node,
        end_byte: u64,
        caller_qn: &str,
        receiver_hint: Option<String>,
    ) -> CallEntry {
        let line = start_node.start_position().row as u64 + 1;
        let col = start_node.start_position().column as u64;
        let start_byte = start_node.start_byte() as u64;
        let qn = format!("{caller_qn}::call@{line}:{col}#{start_byte}-{end_byte}");
        let mut properties = vec![
            ("callee_name".to_string(), callee.to_string()),
            ("caller_qn".to_string(), caller_qn.to_string()),
            // source: LSP 3.17 Base Protocol — positions are 0-based;
            // `col` above is already 0-based here (this spec's QN
            // convention), so `lsp_col` mirrors it verbatim. Read by
            // indexer::persist::nodes::append_label_properties to
            // populate CallSite.col for lsp_resolve's definition queries.
            ("lsp_col".to_string(), col.to_string()),
        ];
        // source: tasks/plan-issues-282-283-284.md §2.3 (lot 6, issue #283
        // palier 3) — omitted (not even an empty-string property) when
        // `None`; `indexer::persist::nodes::append_callsite_properties`
        // already defaults an absent `receiver_hint` property to "".
        if let Some(hint) = receiver_hint {
            properties.push(("receiver_hint".to_string(), hint));
        }
        CallEntry {
            name: callee.to_string(),
            qualified_name: qn.clone(),
            visibility: String::new(),
            properties,
            start_line: line,
            end_line: line,
            ref_kind: "Defines",
            ref_to: qn,
        }
    }
}
