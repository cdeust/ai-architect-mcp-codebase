// parser::spec::walkers::rust_body — the Rust function-BODY walker (issue #327).
//
// A `fn` item may be declared inside a function body (Rust Reference, "Block
// expressions": a block may contain item declarations, visible throughout the
// block). The shared `calls::walk_calls` DFS saw such an item only as more
// body: the nested fn got no node, and every call in its body was attributed
// to the enclosing function. Measured on DYResearch/dy-wcet v4.1.2
// (`fn gcd` inside `TaskSet::passes_utilisation_bound`, `src/lib.rs:1081`).
//
// This walker is the same stack DFS as `walk_calls` (so call sites keep their
// emission order), except that a nested `fn` item is emitted as its own
// `Function` scoped under the enclosing callable's QN, and its body is walked
// with the nested fn as the caller. A fn-local `impl`/`trait`/`mod` is NOT
// entered for items: its `fn`s are members of a type or module this walker
// does not index, so they are walked for calls only, attributed to the
// enclosing callable exactly as before this walker existed.
// source: https://doc.rust-lang.org/reference/expressions/block-expr.html

use tree_sitter::Node;

use super::rust::{has_async, push_def, Def, RustSpecs};
use super::{call_scan_of, calls, kind_in, line_of, type_uses, WalkCtx};
use crate::parser::{node_field_text, LABEL_FUNCTION};

/// Walks `body` (the call-scan body of the callable `caller_qn`): emits its
/// call sites attributed to `caller_qn` and every `fn` item declared in it as
/// a nested `Function`, recursively.
///
/// precondition: `body` is the body node `call_scan_of` returned for the
/// callable whose QN is `caller_qn`.
/// postcondition: every call node under `body` and outside a nested `fn` item
/// yields its call site(s) with `caller_qn` as caller; every nested `fn` item
/// reachable without entering a fn-local `impl`/`trait`/`mod` yields one
/// `Function` node, and its own body is walked by this same function.
pub(super) fn walk_fn_body(specs: RustSpecs, ctx: &mut WalkCtx, body: Node, caller_qn: &str) {
    let rf = specs.rf;
    // `true` while `fn` items at this depth are items of the enclosing
    // callable's body; `false` once inside a fn-local impl/trait/mod.
    let mut stack = vec![(body, true)];
    while let Some((n, items)) = stack.pop() {
        let k = n.kind();
        if items && kind_in(rf.function_kinds, k) {
            emit_nested_function(specs, ctx, n, caller_qn);
            continue;
        }
        calls::emit_call_sites(specs.spec, ctx, n, caller_qn);
        let child_items = items && !is_item_scope(specs, k);
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push((c, child_items));
        }
    }
}

/// Whether `kind` opens a scope whose `fn` members are not items of the
/// enclosing function body: a type (`impl`, `trait`) or a module.
fn is_item_scope(specs: RustSpecs, kind: &str) -> bool {
    let rf = specs.rf;
    kind_in(rf.impl_kinds, kind) || kind_in(rf.trait_kinds, kind) || kind_in(rf.mod_kinds, kind)
}

/// Emits one `fn` item declared inside the body of `enclosing_qn` as a
/// `Function` named `{enclosing_qn}::{name}`, defined by its FILE, then walks
/// its own body with itself as the caller.
///
/// Why these choices:
/// - QN under the enclosing callable: the item is visible only inside that
///   body, so a QN under the file or module would collide with, and be
///   indistinguishable from, a same-named item the whole module can name.
///   `ctx.dedup` separates two same-named fns in sibling blocks of one body.
///   It is not twin-aware (issue #353): two nested fns under mutually exclusive
///   `#[cfg]` are told apart by `qn@line` exactly as before, carry no `cfg_gate`
///   and no `#cfg(..)` suffix, and both stay nodes.
/// - `Function`, never `Method`: an item inside a body has no `self`.
/// - `Defines` from the file: the file does define it, and the schema has no
///   `Defines_Function_Function`/`Defines_Method_Function` table (adding one
///   would break incremental re-index of every existing graph, which skips
///   the DDL pass). The enclosing callable is recoverable from the QN, which
///   is what the resolver's visibility rule reads.
/// - No `entry_kind`: rustc's `unnameable_test_items` lint ("cannot test
///   inner items") means an inner `#[test]` fn is never run by the harness.
///   source: https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html#unnameable-test-items
fn emit_nested_function(specs: RustSpecs, ctx: &mut WalkCtx, node: Node, enclosing_qn: &str) {
    let spec = specs.spec;
    let name = node_field_text(ctx.source, node, spec.name_field);
    if name.is_empty() {
        return;
    }
    let seq = ctx.next_seq();
    let base_qn = spec.conventions.def_qn(enclosing_qn, &name, seq);
    let qn = ctx.dedup(base_qn, line_of(node));
    let mut properties = vec![(
        "is_async".to_string(),
        has_async(specs.rf, node).to_string(),
    )];
    type_uses::append_type_use_props(spec, ctx, node, call_scan_of(spec, node), &mut properties);
    let file = ctx.file_path;
    push_def(
        ctx,
        node,
        Def {
            label: LABEL_FUNCTION,
            name: &name,
            qn: &qn,
            visibility: spec.conventions.node_visibility(ctx.source, node, &name),
            properties,
            edge_kind: "Defines",
            edge_from: file,
        },
    );
    if let Some(body) = call_scan_of(spec, node) {
        walk_fn_body(specs, ctx, body, &qn);
    }
}
