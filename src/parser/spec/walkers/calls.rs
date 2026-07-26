// parser::spec::walkers::calls — the generic call-expression walker.
//
// Split out of `walkers/mod.rs` (issue #101, §4.1) as a pure move; the
// `ExtractedNode`/`ExtractedRef` contract is unchanged.

use tree_sitter::Node;

use super::super::lang_spec::LangSpec;
use super::{kind_in, WalkCtx};
use crate::parser::{ExtractedNode, ExtractedRef, LABEL_CALL_SITE};

/// Call walker: emits one `CallSite` node + edge per call expression the
/// conventions accept. Stack DFS matches the hand-written walker so the `seq`
/// counter (which keys Go call-site QNs) is assigned in identical order. `seq`
/// is consumed only when the callee is accepted, so a dropped call (Go's
/// non-identifier callee) does not perturb the counter.
pub(super) fn walk_calls(spec: &LangSpec, ctx: &mut WalkCtx, root: Node, caller_qn: &str) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if kind_in(spec.call_node_kinds, n.kind()) {
            if let Some(callee) = spec.conventions.call_callee(ctx.source, n) {
                let seq = ctx.next_seq();
                let entry = spec
                    .conventions
                    .call_entry(ctx.source, n, caller_qn, &callee, seq);
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
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}
