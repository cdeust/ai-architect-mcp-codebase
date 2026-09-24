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
//   assert!(helper(x))                              -- the bare-call shape
//     token_tree children (named): identifier(helper) token_tree((x))
//
// The reliable, macro-agnostic signature for "X.method(...)" / "X::method(...)"
// is: two consecutive NAMED `identifier` children immediately followed by a
// `token_tree` child (the reconstructed call's own parenthesized arguments).
//
// A bare call `helper(x)` is a single identifier followed by a token_tree
// (issue #328: `assert_eq!(old_response_of(&s, 1), Some(9))` in dy-wcet
// v4.1.2 produced no CallSite, and nothing reported the drop). The same
// two-node shape also covers things that are not calls, each told apart by a
// token a probe of tree-sitter-rust 0.24.2 shows (2026-09-23, issue #328):
//   vec![1], v!(1)       `!` sits between the name and the group
//   arr[0], else { c }   the group opens with `[` or `{`, not `(`
//   s.0.foo(1), u8::from(1), <T as Tr>::m(1)
//                        the name is a segment after `.` or `::`
//   fn foo(x), struct A(u8)
//                        an item definition, not a call
//   #[cfg(test)]         an attribute, whose token_tree is not scanned
//   for i in (0..3), yield (1)
//                        a keyword lexed as `identifier` inside a token_tree
//   let t = |x| x; t(1)  a name bound in the enclosing function: a closure or
//                        fn pointer, never a graph node, and the resolver binds
//                        a bare name to any same-named function (measured on
//                        v0.12.0), so emitting it would invent a caller.

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

use std::cell::OnceCell;
use std::collections::HashSet;

use tree_sitter::Node;

use super::conventions::CallEntry;
use super::lang_spec::RustFamilySpec;
use super::rust::RustConventions;

/// Entry point: `call_node` is one `RUST_FAMILY.macro_invocation_kind` node
/// already accepted by `walk_calls`'s DFS. Returns one `CallEntry` per bare,
/// method or path call reconstructed from its argument `token_tree`,
/// recursively — a call nested inside another call's arguments
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
    let ctx = ScanCtx {
        source,
        caller_qn,
        token_tree_kind: family.token_tree_kind,
        macro_node: call_node,
        bound: OnceCell::new(),
    };
    let mut out = Vec::new();
    scan_token_tree(&ctx, token_tree, &mut out);
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

/// Rust keywords that can lex as a named `identifier` inside a token_tree
/// (the probe shows `in`, `else`, `move`, `yield`), so a keyword directly
/// followed by `(` (`for i in (0..3)`, `yield (1)`) is not taken for a call.
/// source: The Rust Reference, "Keywords" (strict and reserved keywords,
/// https://doc.rust-lang.org/reference/keywords.html). `gen` is left out: it
/// is reserved only from edition 2024, and a legal function name before.
const RUST_KEYWORDS: [&str; 51] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// Anonymous tokens that, directly before a name, make it something other
/// than a bare call: a method or path segment (`s.0.foo(1)`, `u8::from(1)`,
/// `<T as Tr>::m(1)`), or the name of an item being defined (`fn foo(x)`,
/// `struct A(u8)`). source: tree-sitter-rust 0.24.2 probe, 2026-09-23.
const NOT_A_CALL_AFTER: [&str; 4] = [".", "::", "fn", "struct"];

/// The delimiter that opens a call's argument group.
/// source: tree-sitter-rust 0.24.2 probe, 2026-09-23 (`[`/`{` groups follow
/// an index or a block, never a call).
const CALL_ARGS_OPEN: &str = "(";

/// The token that opens an attribute (`#[..]`), and the one that makes it an
/// inner attribute (`#![..]`).
/// source: tree-sitter-rust 0.24.2 probe, 2026-09-23.
const ATTRIBUTE_MARK: &str = "#";
const INNER_ATTRIBUTE_MARK: &str = "!";

/// What one scan of a macro's argument payload reads, shared by every
/// recursion level.
struct ScanCtx<'a> {
    source: &'a str,
    caller_qn: &'a str,
    token_tree_kind: &'a str,
    macro_node: Node<'a>,
    /// Names bound in the enclosing function, computed only once a bare-call
    /// candidate reaches that check, so a macro without one never walks the
    /// function for its bindings.
    bound: OnceCell<HashSet<String>>,
}

impl ScanCtx<'_> {
    fn is_bound(&self, name: &str) -> bool {
        self.bound
            .get_or_init(|| super::rust_scope::bound_names_in_scope(self.source, self.macro_node))
            .contains(name)
    }
}

/// Scans `token_tree`'s NAMED children left to right for three call shapes:
/// `[identifier, identifier, token_tree]` and `[token_tree, identifier,
/// token_tree]` joined by `.`/`::` (`separated_by_dot_or_colon`), and a bare
/// `identifier` directly followed by a `(` group (`is_bare_call`). Each match
/// advances ONTO the reconstructed call's own argument token_tree, so a
/// method chained on the call's RESULT still matches next, and that
/// token_tree is recursed into exactly once, by whichever arm consumes it.
/// Every other `token_tree` child is recursed into, except an attribute's.
fn scan_token_tree(ctx: &ScanCtx, token_tree: Node, out: &mut Vec<CallEntry>) {
    let mut cursor = token_tree.walk();
    let named: Vec<Node> = token_tree.named_children(&mut cursor).collect();
    let mut i = 0;
    while i < named.len() {
        let trailing_args = i + 2 < named.len() && named[i + 2].kind() == ctx.token_tree_kind;
        let joined = trailing_args
            && named[i + 1].kind() == IDENTIFIER_KIND
            && separated_by_dot_or_colon(ctx.source, named[i], named[i + 1]);

        // `X.method(...)` / `X::method(...)`: the receiver is a plain name.
        if joined && named[i].kind() == IDENTIFIER_KIND {
            push_reconstructed(ctx, named[i], named[i + 1], out);
            i += 2;
            continue;
        }

        // `f(...).method(...)`: the receiver is a CALL RESULT, so it is a
        // token_tree rather than an identifier.
        // source: ADR-9836.
        if joined && named[i].kind() == ctx.token_tree_kind {
            scan_token_tree(ctx, named[i], out);
            push_reconstructed(ctx, named[i], named[i + 1], out);
            i += 2;
            continue;
        }

        // `f(...)`: a bare call (issue #328).
        if is_bare_call(ctx, named[i]) {
            push_bare_call(ctx, named[i], out);
        } else if named[i].kind() == ctx.token_tree_kind && !is_attribute(named[i]) {
            scan_token_tree(ctx, named[i], out);
        }
        i += 1;
    }
}

/// True when `name` is an unbound, non-keyword `identifier` that directly
/// precedes a `(` group and is not a path/method segment or an item name.
fn is_bare_call(ctx: &ScanCtx, name: Node) -> bool {
    if name.kind() != IDENTIFIER_KIND {
        return false;
    }
    // Only a token_tree has a `(` child, so this alone proves the next
    // sibling is a parenthesised group (a `!` in between fails it).
    let args_follow = name
        .next_sibling()
        .and_then(|args| args.child(0))
        .is_some_and(|open| open.kind() == CALL_ARGS_OPEN);
    let after_non_call = name
        .prev_sibling()
        .is_some_and(|prev| NOT_A_CALL_AFTER.contains(&prev.kind()));
    if !args_follow || after_non_call {
        return false;
    }
    let text = &ctx.source[name.start_byte()..name.end_byte()];
    !RUST_KEYWORDS.contains(&text) && !ctx.is_bound(text)
}

/// True when `token_tree` is an attribute's body (`#[..]` or `#![..]`):
/// attribute arguments (`cfg(test)`, `derive(Debug)`) are not calls.
fn is_attribute(token_tree: Node) -> bool {
    let Some(prev) = token_tree.prev_sibling() else {
        return false;
    };
    prev.kind() == ATTRIBUTE_MARK
        || (prev.kind() == INNER_ATTRIBUTE_MARK
            && prev
                .prev_sibling()
                .is_some_and(|mark| mark.kind() == ATTRIBUTE_MARK))
}

/// Emits one bare call named by `name`. No receiver hint: a bare call has no
/// receiver, the same as `extra_call_entries`'s by-value sites.
fn push_bare_call(ctx: &ScanCtx, name: Node, out: &mut Vec<CallEntry>) {
    let callee = &ctx.source[name.start_byte()..name.end_byte()];
    out.push(RustConventions::call_site_spanning(
        callee,
        name,
        name.end_byte() as u64,
        ctx.caller_qn,
        None,
    ));
}

/// Emits one reconstructed call spanning `receiver` through `method`.
///
/// postcondition: the name is a CONTIGUOUS slice of `source` ending on the
/// method identifier, which `lsp_resolver::sites::lsp_position` relies on to
/// aim at the right column. source: ADR-9836.
///
/// `receiver` is ALREADY the isolated receiver `identifier` node (this
/// scan's own match, not a `field_expression`-wrapped one), so it is exactly
/// the node shape `rust_receiver::receiver_hint`'s `identifier` arm expects
/// — issue #283 palier 3 (lot 6) reuses it directly rather than re-deriving
/// it from the reconstructed call's (nonexistent) `call_expression`.
fn push_reconstructed(ctx: &ScanCtx, receiver: Node, method: Node, out: &mut Vec<CallEntry>) {
    let callee = ctx.source[receiver.start_byte()..method.end_byte()].to_string();
    if callee.is_empty() {
        return;
    }
    let derived = super::rust_receiver::receiver_hint_with_origin(ctx.source, receiver);
    out.push(RustConventions::with_hint_origin(
        &callee,
        receiver,
        method.end_byte() as u64,
        ctx.caller_qn,
        derived,
    ));
}
