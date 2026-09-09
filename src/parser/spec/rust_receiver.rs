// parser::spec::rust_receiver — the parser-side half of issue #283 palier 3
// (lot 6): attaches a `receiver_hint` to a Rust CallSite when its receiver is
// a simple local identifier bound exactly once in the enclosing function.
//
// source: tasks/plan-issues-282-283-284.md §2.2 palier 3 / §2.3 (lot 6) and
// ADR-<pending> (content in the PR body pending publication — coordinator
// note 2026-09-09).
//
// THE SPLIT: `rust_scope::typed_local_bindings` owns "what names are bound
// in this scope, and with what simplified type" (a scope/binding concern
// shared with issue #87's `bound_names_in_scope`, same traversal, richer
// read). This module owns only the call-site-shaped question: "does THIS
// call's receiver spell a plain local identifier at all", and if so, looks
// its name up in that map. The two together are `receiver_hint`.
//
// TWO CALL SHAPES, ONE ENTRY POINT: a real `call_expression` (`s.m()`) has
// the receiver buried inside a `field_expression`'s `value` field; a
// macro-argument-reconstructed call (`rust_macro_calls`, `assert_eq!(s.m(),
// ..)`) already hands over the receiver `identifier` node directly (no
// `call_expression` exists for it — tree-sitter never expands macros).
// `receiver_identifier` accepts either node shape so `rust.rs` and
// `rust_macro_calls.rs` can both call `receiver_hint` the same way.

use tree_sitter::Node;

use crate::parser::node_text;

/// The `function` field name on `call_expression`. source: tree-sitter-rust
/// 0.24.2 src/node-types.json.
const FUNCTION_FIELD: &str = "function";
/// The `value` field name on `field_expression` — the receiver expression of
/// `value.field`.
const VALUE_FIELD: &str = "value";

/// precondition: `node` is either a `call_expression` (the callee's own
/// call node) or an `identifier` (a macro-reconstructed call's receiver,
/// already isolated by `rust_macro_calls::scan_token_tree`) that sits
/// somewhere inside a parsed Rust function/closure body.
/// postcondition: `Some(T)` iff `node`'s receiver is a plain local
/// identifier bound EXACTLY ONCE in the enclosing scope by a parameter or
/// `let` whose type is determinable (plan §2.2 palier 3) — `T` is that
/// type's last `::` segment with generics stripped. `None` for every other
/// shape: a chained/indexed/field-access receiver (`self.tasks.get`,
/// `sets[0].response_of`, `x.trim().len`), a `self`/`Self` receiver
/// (paliers 1-2 own those, gated to `Method` callers only — this palier
/// applies to ANY caller, so it deliberately does not special-case `self`;
/// `self`'s node kind is `self`, never `identifier`, so it structurally
/// never reaches `typed_local_bindings`' lookup), or a receiver not bound
/// exactly once with a typed simple pattern.
pub(super) fn receiver_hint(source: &str, node: Node) -> Option<String> {
    let receiver = receiver_identifier(node)?;
    let name = node_text(source, receiver);
    if name.is_empty() {
        return None;
    }
    // `node` and `receiver` share the same enclosing function/closure (the
    // latter is a descendant of the former when `node` is a
    // `call_expression`, or the same node when `node` is already the
    // receiver identifier) — either walks `enclosing_scope` to the same
    // ancestor, so looking the name up via `node` is exact, not a guess.
    super::rust_scope::typed_local_bindings(source, node).remove(&name)
}

/// The receiver's `identifier` node, when `node`'s shape names one plainly:
///   - `node` itself, when it already IS an `identifier` (the
///     macro-reconstructed-call shape).
///   - `node.function.value`, when `node` is a `call_expression` whose
///     callee is `field_expression` (`x.m(...)`) AND that field_expression's
///     `value` is itself a plain `identifier` — a chain (`x.trim().len()`),
///     an index (`sets[0].response_of()`), or another field access
///     (`self.tasks.get()`) all fail this last check and return `None`.
fn receiver_identifier(node: Node) -> Option<Node> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    if node.kind() != "call_expression" {
        return None;
    }
    let function = node.child_by_field_name(FUNCTION_FIELD)?;
    if function.kind() != "field_expression" {
        return None;
    }
    let value = function.child_by_field_name(VALUE_FIELD)?;
    if value.kind() == "identifier" {
        Some(value)
    } else {
        None
    }
}
