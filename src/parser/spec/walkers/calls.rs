// parser::spec::walkers::calls — the generic call-expression walker.
//
// Split out of `walkers/mod.rs` (issue #101, §4.1) as a pure move; the
// `ExtractedNode`/`ExtractedRef` contract is unchanged.

use tree_sitter::Node;

use super::super::conventions::CallEntry;
use super::super::lang_spec::LangSpec;
use super::{kind_in, WalkCtx};
use crate::parser::{ExtractedNode, ExtractedRef, LABEL_CALL_SITE};

/// Call walker: emits the `CallSite` node(s) + edge(s) per call expression the
/// conventions accept. Stack DFS matches the hand-written walker so the `seq`
/// counter (which keys Go call-site QNs) is assigned in identical order. `seq`
/// is consumed only when the callee is accepted, so a dropped call (Go's
/// non-identifier callee) does not perturb the counter.
///
/// One accepted call node yields the `call_entry` site FIRST and then any
/// `extra_call_entries` in order — the shape Rust's higher-order-argument
/// capture needs (`run_all(validate, arg)` → the `run_all` site, then one site
/// per function-value argument), and a no-op for every other language.
pub(super) fn walk_calls(spec: &LangSpec, ctx: &mut WalkCtx, root: Node, caller_qn: &str) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if kind_in(spec.call_node_kinds, n.kind()) {
            if let Some(callee) = spec.conventions.call_callee(ctx.source, n) {
                let seq = ctx.next_seq();
                let entry = spec
                    .conventions
                    .call_entry(ctx.source, n, caller_qn, &callee, seq);
                push_call(ctx, entry, caller_qn);
                for extra in spec
                    .conventions
                    .extra_call_entries(ctx.source, n, caller_qn)
                {
                    push_call(ctx, extra, caller_qn);
                }
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

/// Appends one shaped `CallEntry` as a `CallSite` node plus its outgoing edge.
/// precondition: `entry` was produced by the conventions for `caller_qn`.
/// postcondition: exactly one node and one ref are appended to `ctx`.
fn push_call(ctx: &mut WalkCtx, entry: CallEntry, caller_qn: &str) {
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CALL_SITE.to_string(),
        name: entry.name,
        qualified_name: entry.qualified_name,
        start_line: entry.start_line,
        end_line: entry.end_line,
        visibility: entry.visibility,
        properties: entry.properties,
    });
    ctx.refs.push(ExtractedRef {
        kind: entry.ref_kind.to_string(),
        from_qualified_name: caller_qn.to_string(),
        to_qualified_name: entry.ref_to,
    });
}
