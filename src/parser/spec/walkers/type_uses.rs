// parser::spec::walkers::type_uses — shared extraction of the type-usage
// properties (return-type annotation + type-construction expressions) that the
// resolver's Uses phase binds to `Uses_<caller>_<Type>` edges (issue #92).
//
// This is the "implement once at the spec level" half of #92: one helper, gated
// entirely by `LangSpec` data (`return_type_field`, `type_construction_kinds`,
// `construction_type_field`), invoked by all three def-walker lanes (the generic
// `defs` lane for Go, the `rust` lane, the `typescript` lane). A language adopts
// the feature by populating those spec fields; a language that leaves them unset
// (an absent field / an empty slice) gets no property appended and therefore no
// behavior change — the OCP contract the untouched-language parity suites prove.
//
// The mechanism mirrors the existing `Field.type_annotation` → `Uses_Field_*`
// pipeline: the walker records the reference as a NODE PROPERTY (a raw type
// string), because the resolver re-derives name→target bindings from node
// properties server-side, after every node is indexed — a raw `ExtractedRef`
// whose target is a bare NAME (not a QN) is dropped by the indexer's
// `resolve_edge_table` (only structural refs become edges directly).

use std::collections::HashSet;

use tree_sitter::Node;

use super::super::lang_spec::LangSpec;
use super::{kind_in, WalkCtx};
use crate::parser::node_text;

/// Node property key: the function/method return-type annotation text.
/// Read by the resolver's `resolve_callable_type_uses`.
pub(super) const RETURN_TYPE_PROP: &str = "return_type";
/// Node property key: the space-joined, first-seen-order-deduplicated set of
/// constructed type names in a function/method body. Read by the resolver.
pub(super) const CONSTRUCTED_TYPES_PROP: &str = "constructed_types";

/// Appends the type-usage properties to `props` for one function/method node,
/// each gated by the spec so a non-adopting language appends nothing.
///
/// - `def_node` is the function/method declaration (its `return_type_field`
///   child, when the spec names one, is the return-type annotation).
/// - `body` is the call-scan body node (the same node the call walker scans),
///   or `None` for a bodiless def (an abstract method / a trait requirement) —
///   which constructs nothing, so no `constructed_types` property is added.
///
/// precondition: `props` is the callable's property vector under construction.
/// postcondition: at most two entries are appended — `return_type` iff the spec
/// names a `return_type_field` AND the field yields non-empty text, and
/// `constructed_types` iff the spec lists `type_construction_kinds` AND the body
/// contains at least one such node with a non-empty `construction_type_field`.
/// An empty result appends nothing (keeping a non-adopting language's node
/// properties byte-identical to before — the parity invariant).
pub(super) fn append_type_use_props(
    spec: &LangSpec,
    ctx: &WalkCtx,
    def_node: Node,
    body: Option<Node>,
    props: &mut Vec<(String, String)>,
) {
    if let Some(field) = spec.return_type_field {
        let text = return_type_text(ctx.source, def_node, field);
        if !text.is_empty() {
            props.push((RETURN_TYPE_PROP.to_string(), text));
        }
    }
    if !spec.type_construction_kinds.is_empty() {
        if let (Some(body), Some(type_field)) = (body, spec.construction_type_field) {
            let joined = constructed_types_text(spec, ctx.source, body, type_field);
            if !joined.is_empty() {
                props.push((CONSTRUCTED_TYPES_PROP.to_string(), joined));
            }
        }
    }
}

/// The return-type annotation text of `def_node`'s `field` child, with a leading
/// TypeScript-style `:` or Rust-style `->` and surrounding whitespace trimmed so
/// the stored text is the bare type (`OrderConfig`, `Vec<Foo>`, `(int, error)`).
/// Empty when the field is absent (a function with no declared return type).
/// The resolver's `extract_type_identifiers` does the generic/ref stripping;
/// this only removes the annotation lead-in the grammar keeps in the field text
/// (tree-sitter-typescript's `return_type` is a `type_annotation` node whose text
/// is `": T"`).
fn return_type_text(source: &str, def_node: Node, field: &str) -> String {
    let raw = match def_node.child_by_field_name(field) {
        Some(n) => node_text(source, n),
        None => return String::new(),
    };
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("->")
        .or_else(|| trimmed.strip_prefix(':'))
        .unwrap_or(trimmed);
    stripped.trim().to_string()
}

/// The space-joined, first-seen-order-deduplicated constructed-type names found
/// by a stack DFS of `body` for `type_construction_kinds` nodes, reading each
/// node's `type_field` child text. Matches the call walker's whole-body DFS
/// (constructions inside nested closures/blocks attribute to the enclosing named
/// callable, exactly as calls do). Space is a safe join separator: the resolver
/// splits on whitespace anyway, so a generic construction text (`Wrapper<Foo>`)
/// survives intact.
fn constructed_types_text(spec: &LangSpec, source: &str, body: Node, type_field: &str) -> String {
    let mut seen: HashSet<String> = HashSet::new();
    let mut ordered: Vec<String> = Vec::new();
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if kind_in(spec.type_construction_kinds, n.kind()) {
            if let Some(tn) = n.child_by_field_name(type_field) {
                let text = node_text(source, tn);
                let text = text.trim().to_string();
                if !text.is_empty() && seen.insert(text.clone()) {
                    ordered.push(text);
                }
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    ordered.join(" ")
}
