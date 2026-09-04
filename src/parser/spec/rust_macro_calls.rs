// parser::spec::rust_macro_calls — the macro-argument call-site scan.
//
// Split out of `rust.rs` to keep it under the §4.1 500-line cap (the same
// precedent as `kotlin.rs` / `kotlin_conventions.rs`); this module houses the
// scan's algorithm and rationale, `rust.rs` keeps only the one-line trait
// delegation.
//
// THE GAP: tree-sitter does not expand macros. A macro invocation's argument
// list parses as a `token_tree` — a flat bag of raw tokens (identifiers,
// literals, punctuation, and nested `token_tree`s for any balanced-delimiter
// group, `(...)` included) — never as `call_expression` nodes. So
// `assert_eq!(s.slack_of(1), None)` yields exactly one `CallSite` (for
// `assert_eq!` itself, via `RUST_SPEC.call_node_kinds`'s `macro_invocation`
// arm); `s.slack_of(1)` is invisible to `walk_calls`'s DFS because there is
// structurally nothing of a matching kind inside the token_tree for it to
// find — not unresolved, not even represented as a call at all. Any macro
// whose arguments contain call-shaped syntax has the identical blind spot
// (`println!("{}", s.response_of(i))`, `format!(...)`, a custom macro); the
// assert family is where this was empirically measured (DYResearch/dy-wcet:
// 96 assert-family invocations, at least 46 containing a real method call),
// not the boundary of where the gap exists — so this scan fires on ANY
// `macro_invocation`, never gated to a macro-name allowlist.
//
// THE FIX: `.` and `::` are anonymous tokens in this grammar — invisible to
// `node.kind()` matching and to `named_children()` — but the byte range
// between the two identifiers straddling one is still readable straight from
// source. Verified directly against tree-sitter-rust 0.24.2 (Cargo.lock-pinned)
// by probing `tree_sitter_rust::LANGUAGE` on dy-wcet's own shapes plus several
// adversarial ones (see PR description for the full probe transcript):
//
//   assert_eq!(s.slack_of(1), None)
//     token_tree children (named): identifier(s) identifier(slack_of)
//                                   token_tree((1)) identifier(None)
//   assert!(s.is_schedulable())
//     token_tree children (named): identifier(s) identifier(is_schedulable)
//                                   token_tree(())
//   assert!(helper(x))                              -- must NOT match
//     token_tree children (named): identifier(helper) token_tree((x))
//
// The reliable, macro-agnostic signature for "X.method(...)" / "X::method(...)"
// is: two consecutive NAMED `identifier` children immediately followed by a
// `token_tree` child (the reconstructed call's own parenthesized arguments).
// A single identifier directly followed by a `token_tree` — a bare function
// call (`helper(x)`), OR a nested macro invocation with its own token_tree
// payload (`vec![...]`) — deliberately does NOT match: with only one
// identifier there is no way to distinguish "plain call" from "receiver
// missing", so it is left alone rather than guessed, matching the
// `extra_call_entries` (issue #87) precedent of never emitting a site the
// scan cannot back with real evidence.
//
// KNOWN GAP (not fixed here — an issue candidate, not a silent
// mis-extraction): a turbofish (`s.parse::<i32>(1)`) interposes a named type
// node (`primitive_type` / `type_identifier` / …) between the second
// identifier and the call's `token_tree`, so this pattern does not match it.
// The call is simply not extracted, not mis-extracted — the same honesty
// discipline `extra_call_entries` already applies to a local-binding argument
// the resolver cannot bind.
//
// source: tree-sitter-rust 0.24.2 src/node-types.json — `macro_invocation`
// declares exactly one non-field child, of kind `token_tree`, `required: true,
// multiple: false` (verified 2026-09-04 via `python3 -c 'import json; ...'`
// against the Cargo.lock-pinned crate source).

use tree_sitter::Node;

use super::conventions::CallEntry;
use super::lang_spec::RustFamilySpec;
use super::rust::RustConventions;

/// Entry point: `call_node` is one `RUST_FAMILY.macro_invocation_kind` node
/// already accepted by `walk_calls`'s DFS. Returns one `CallEntry` per
/// method/path call reconstructed from its argument `token_tree`, recursively
/// — a call nested inside another call's arguments
/// (`assert!(s.method(a.other()))`) or inside a sibling macro's own
/// token_tree (`assert!(vec![s.method()].len() > 0)`) is still found, because
/// every `token_tree` encountered is recursed into regardless of whether it
/// matched as a reconstructed call's own arguments.
///
/// precondition: `call_node` is a node the caller already confirmed is a
/// `call_node_kinds` match; this function itself re-checks the macro-kind
/// gate so it is safe to call for a `call_expression` node too (returns
/// empty — a `call_expression` has no `macro_invocation_kind`-shaped
/// argument payload for this scan to walk).
/// postcondition: every returned `CallEntry` carries a real callee text byte-
/// sliced from `source` (never empty) and a `qualified_name` unique among
/// this caller's call sites (the same `{caller_qn}::call@{line}:{col}#{span}`
/// scheme `RustConventions::call_site` uses elsewhere).
pub(super) fn macro_argument_call_entries(
    source: &str,
    call_node: Node,
    caller_qn: &str,
    family: &RustFamilySpec,
) -> Vec<CallEntry> {
    if call_node.kind() != family.macro_invocation_kind {
        return Vec::new();
    }
    let token_tree = match direct_child_of_kind(call_node, family.token_tree_kind) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    scan_token_tree(
        source,
        token_tree,
        caller_qn,
        family.token_tree_kind,
        &mut out,
    );
    out
}

/// The macro invocation's sole `token_tree` child (its grammar-declared
/// argument payload — not a named field, but the node-types.json entry
/// guarantees exactly one such child).
fn direct_child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// Named-child identifier kind. Not a `RustFamilySpec` field: `"identifier"`
/// is the fixed tree-sitter-rust leaf kind for every bare name in this
/// grammar (already relied on verbatim elsewhere, e.g.
/// `RUST_FAMILY.fn_value_arg_kinds`), not a per-language-row convention that
/// varies — the same local-const precedent `kotlin_conventions.rs` uses for
/// grammar facts read only by one conventions module.
/// source: tree-sitter-rust 0.24.2 src/node-types.json (`identifier`).
const IDENTIFIER_KIND: &str = "identifier";

/// True when `first` and `second` are separated in `source` by exactly `.`
/// or `::` (after trimming whitespace) — the only separators that make two
/// adjacent identifiers a genuine "X.method" / "X::method" pair.
///
/// Two SEPARATE macro arguments where the second happens to be a bare
/// identifier immediately followed by a call — `assert!(flag,
/// format_error(ctx))` — present the identical
/// `[identifier, identifier, token_tree]` shape as a real receiver call, but
/// the source bytes between them read `, ` (comma), never `.`/`::`. Comma is
/// just as anonymous in this grammar as `.`/`::` (verified 2026-09-04
/// alongside the `.`/`::` probe already on file), so the node-kind shape
/// alone cannot distinguish the two cases — only the literal separator text
/// can, which is what this check reads.
fn separated_by_dot_or_colon(source: &str, first: Node, second: Node) -> bool {
    matches!(
        source[first.end_byte()..second.start_byte()].trim(),
        "." | "::"
    )
}

/// Scans `token_tree`'s NAMED children left to right for the
/// `[identifier, identifier, token_tree]` shape joined by `.`/`::`
/// (`separated_by_dot_or_colon`), non-overlapping (a matched triple's three
/// children are consumed together, so the scan resumes just past the
/// reconstructed call's own arguments rather than re-testing inside them as
/// a fresh window). Every `token_tree` child — whether it matched as a
/// reconstructed call's arguments or not — is recursed into, so nested calls
/// and sibling macro invocations are still scanned for calls of their own.
fn scan_token_tree(
    source: &str,
    token_tree: Node,
    caller_qn: &str,
    token_tree_kind: &str,
    out: &mut Vec<CallEntry>,
) {
    let mut cursor = token_tree.walk();
    let named: Vec<Node> = token_tree.named_children(&mut cursor).collect();
    let mut i = 0;
    while i < named.len() {
        let shaped = i + 2 < named.len()
            && named[i].kind() == IDENTIFIER_KIND
            && named[i + 1].kind() == IDENTIFIER_KIND
            && named[i + 2].kind() == token_tree_kind;
        if shaped && separated_by_dot_or_colon(source, named[i], named[i + 1]) {
            let callee = source[named[i].start_byte()..named[i + 1].end_byte()].to_string();
            if !callee.is_empty() {
                out.push(RustConventions::call_site_spanning(
                    &callee,
                    named[i],
                    named[i + 1].end_byte() as u64,
                    caller_qn,
                ));
            }
            scan_token_tree(source, named[i + 2], caller_qn, token_tree_kind, out);
            i += 3;
            continue;
        }
        if named[i].kind() == token_tree_kind {
            scan_token_tree(source, named[i], caller_qn, token_tree_kind, out);
        }
        i += 1;
    }
}
