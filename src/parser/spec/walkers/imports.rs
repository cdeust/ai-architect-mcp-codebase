// parser::spec::walkers::imports — the generic import-statement walker.
//
// Split out of `walkers/mod.rs` (issue #101, §4.1) as a pure move; the
// `ExtractedNode`/`ExtractedRef` contract is unchanged.

use tree_sitter::Node;

use super::super::lang_spec::LangSpec;
use super::WalkCtx;
use crate::parser::{ExtractedNode, ExtractedRef, LABEL_IMPORT};

/// Import walker: interprets one import statement into zero or more `Import`
/// nodes + import edges via the conventions (Go descends to `import_spec`
/// nodes; Python dispatches the three Python import-statement kinds).
pub(super) fn walk_imports(spec: &LangSpec, ctx: &mut WalkCtx, import_node: Node, scope: &str) {
    for entry in spec
        .conventions
        .imports_of(ctx.source, spec, import_node, scope)
    {
        ctx.nodes.push(ExtractedNode {
            label: LABEL_IMPORT.to_string(),
            name: entry.display_name,
            qualified_name: entry.qualified_name,
            start_line: entry.start_line,
            end_line: entry.end_line,
            visibility: entry.visibility,
            properties: entry.properties,
        });
        ctx.refs.push(ExtractedRef {
            kind: spec.conventions.import_ref_kind(import_node).to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: entry.ref_to,
        });
    }
}
