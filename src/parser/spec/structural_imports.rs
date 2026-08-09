// parser::spec::structural_imports — issue #224's last named wall: generic,
// zero-per-language import extraction for the grammar-introspection engine.
//
// ## The insight (verified empirically against 20 real grammars below, not
// assumed): imports resist FIELD-shape classification (`structural.rs`'s
// original module doc measured 0/6 — Go `path`, TS `source`, Rust `argument`,
// Python none, Java zero fields, no shared convention) because an import
// STATEMENT's shape is genuinely different per grammar author. But the
// KEYWORD that introduces it is not a design choice — every sampled grammar
// spells it out as an actual token, and tree-sitter always marks a grammar's
// own keywords as ANONYMOUS (unnamed) leaf nodes, whose `kind()` IS the exact
// keyword text (verified: C's `#include` token literally has `kind() ==
// "#include"`; Go's `import` token has `kind() == "import"`). So instead of
// asking "what fields does this node have", this module asks "does this node
// contain an anonymous child whose KIND is a known import keyword" — a
// lexical invariant, not a structural one, and the one thing every
// import-introducing construct in every sampled grammar actually shares.
//
// ## Verification method (issue #224 discipline: read the real
// `tree_sitter::Node` shape, never the grammar's prose docs) — every claim
// below was produced by a throwaway `#[cfg(test)]` tree-dump harness (same
// discipline `structural_fallback.rs` used and later deleted) against 20
// real grammars: the 10 hand-written languages + Ruby (imports table),
// PLUS 9 of the 12 out-of-sample wide-sample grammars (C#, PHP, Scala,
// Haskell, Lua, JavaScript, Julia, Dart, OCaml) reachable via a keyword or
// call-name construct. Elixir, Bash and Zig's import-equivalents are
// verified NOT reachable by this design and documented in "what remains
// unreachable" below — not silently dropped.
//
// ## The five global tables (all languages, no per-language row)
//
// 1. `IMPORT_KEYWORD_TOKENS` — the anonymous-child kind that anchors an
//    import statement. `"from"` is DELIBERATELY excluded even though the
//    owner's starting list included it: verified false-positive risk on
//    Python's `yield ... from ...` and `raise ... from ...`, both of which
//    also carry a direct anonymous `"from"` child (see this module's test
//    `keyword_from_is_excluded_because_python_yield_from_also_carries_it`).
//    `"import"` alone already anchors every Python import shape (both
//    `import_statement` and `import_from_statement` carry an anonymous
//    `"import"` token too), so dropping `"from"` costs nothing and removes a
//    verified false positive — reported explicitly per the task's own
//    "if you add/drop a keyword, say so" requirement.
// 2. `PATH_FIELD_CANDIDATES` — the field name that, when present exactly
//    ONCE on an entry node, holds the import path directly: Go `path`,
//    TypeScript/JavaScript `source`, Rust `argument`, Python's
//    `import_from_statement` `module_name`, Haskell/OCaml `module`. `"name"`
//    is DELIBERATELY excluded from this table (unlike the task brief's
//    suggestion) because it plays TWO CONTRADICTORY roles across grammars —
//    Go's `import_spec.name` is the LOCAL ALIAS, not the path, while
//    Python's `import_statement.name` IS the path — an unresolvable
//    ambiguity for a single global table. Go's alias role is instead handled
//    explicitly in `import_alias_text` (see below).
// 3. `ALIAS_FIELD_CANDIDATES` — `"alias"`, verified on Rust's
//    `use_as_clause`, Python's `aliased_import`, Dart's
//    `import_specification`, Haskell's `import`.
// 4. `AS_KEYWORD` — `"as"`, the anonymous token whose FOLLOWING named
//    sibling is the alias when no `alias` field exists at all (a bounded
//    fallback, not the primary mechanism).
// 5. `IMPORT_CALL_NAMES` — Ruby's `require`/`require_relative` and Lua's
//    `require` are NOT keyword-anchored constructs at all: verified, both
//    parse as an ordinary `call`/`function_call` node (`method`+`arguments`
//    or `name`+`arguments` fields) that TIER 1's existing `call_callee_field`
//    ALREADY classifies as a `CallSite`. This table lets a classified call
//    ALSO emit an `Import` when its resolved callee name matches — still one
//    small global table, never a per-language special case.
//
// ## What this module does NOT reach (verified, not hidden)
//   - **Elixir**: `import Enum` parses as a `call` node with FIELD `target`
//     (not `function`/`method`/`name`), which TIER 1's `call_callee_field`
//     does not recognize at all — Elixir's calls are already wholly
//     unreached by this engine (pre-existing, see `structural_wide_sample_tests`),
//     so its import-as-macro-call form is unreached for the same underlying
//     reason, not a new gap.
//   - **Zig**: `@import("std")` is a `builtin_function` node with NEITHER a
//     `function`/`method`/`name` field (so it's not a recognized call) NOR an
//     anonymous keyword child (`@import` is a single token of kind
//     `"builtin_identifier"`'s parent `builtin_function`, not an anonymous
//     leaf) — genuinely outside both mechanisms this module adds.
//   - **Bash**: `source ./foo.sh` is a `command` node (`name`+`argument`
//     fields, singular `argument`) — the same one-character gap
//     `structural_wide_sample_tests` already documents and deliberately
//     leaves unpatched for calls; it is not a keyword-anchored or
//     `call_callee_field`-shaped construct either, so it stays unreached
//     here too, for the same already-accepted reason.

use tree_sitter::Node;

use crate::parser::node_text;

/// Anonymous-child KIND table that anchors an import statement. See module
/// doc item 1 for why `"from"` is excluded.
const IMPORT_KEYWORD_TOKENS: [&str; 8] = [
    "import", "use", "using", "include", "#include", "#import", "require", "open",
];

/// Field names that, checked in order, hold the import path directly when
/// present exactly once on a candidate node. See module doc item 2.
const PATH_FIELD_CANDIDATES: [&str; 5] = ["path", "source", "argument", "module_name", "module"];

/// See module doc item 3.
const ALIAS_FIELD_CANDIDATES: [&str; 1] = ["alias"];

/// See module doc item 4.
const AS_KEYWORD: &str = "as";

/// See module doc item 5: call names that mark an ALREADY-classified
/// `CallSite` as also being an import (Ruby's/Lua's `require`-family).
/// `"include"` is deliberately NOT in this table even though Ruby has a
/// `include SomeModule` mixin call, because it is a module-inclusion
/// mechanic, not a file/module IMPORT in this graph's sense, and adding it
/// was not verified necessary by any real failing case.
pub(super) const IMPORT_CALL_NAMES: [&str; 3] = ["require", "require_relative", "load"];

/// Delimiter characters stripped from an extracted path/alias — lexical
/// cleanup (a string literal's or a C-family system-include's outer
/// delimiters), not a language rule. Quote chars mirror `shallow.rs`'s
/// identical constant (not shared across modules: the coding-standards.md
/// §3.3 three-use threshold isn't crossed by these two independent call
/// sites); `<`/`>` are added and verified necessary by C/C++/ObjC's
/// `#include <stdio.h>`/`#import <Foundation/Foundation.h>` system-include
/// syntax, whose `path` field node's own text includes the angle brackets.
const QUOTE_CHARS: [char; 5] = ['"', '\'', '`', '<', '>'];

const BOUNDED_SEARCH_DEPTH: u8 = 4;

fn text(source: &str, node: Node) -> String {
    node_text(source, node)
}

fn strip_quotes(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| QUOTE_CHARS.contains(&c))
        .trim()
        .to_string()
}

/// Whether `node` itself carries a DIRECT anonymous child whose kind is an
/// import keyword (module doc item 1). Deliberately direct children only —
/// verified across all sampled grammars, the keyword always sits directly on
/// the outermost import statement node, never on a nested wrapper (Rust's
/// `use_as_clause`, Python's `aliased_import` carry no keyword child of their
/// own — see the module doc and this file's tests).
pub(super) fn is_import_anchor(node: Node) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| !c.is_named() && IMPORT_KEYWORD_TOKENS.contains(&c.kind()));
    found
}

/// True when an ANCESTOR of `node` is itself an import anchor — a defensive
/// guard against double-counting should some future grammar nest one
/// keyword-anchored node inside another (not observed in any sampled
/// grammar, but cheap to rule out structurally rather than assume away).
pub(super) fn has_import_anchor_ancestor(node: Node) -> bool {
    let mut current = node.parent();
    let mut depth = 0u8;
    while let Some(n) = current {
        if depth >= 20 {
            break;
        }
        if is_import_anchor(n) {
            return true;
        }
        current = n.parent();
        depth += 1;
    }
    false
}

/// Splits one import-anchor node into its per-entry sub-nodes: a bounded
/// search for descendants (including the anchor itself) carrying any
/// `PATH_FIELD_CANDIDATES` field, stopping at the first match on each
/// branch (so a matched entry's OWN nested fields are never treated as a
/// second, deeper entry). Falls back to `[anchor]` when nothing in the
/// subtree carries a path field at all — the fieldless-grammar case (Java,
/// Swift, kotlin-ng, C#, PHP), where the whole anchor IS the one entry.
///
/// This one bounded search naturally produces: Go's multiple `import_spec`
/// nodes under `import_spec_list` (two entries), Rust/TypeScript/C/C++/
/// ObjC/Haskell/OCaml's single field-bearing anchor (one entry, matched
/// immediately), and Java/Swift/kotlin-ng/C#/PHP/Scala's fieldless-or-
/// repeated-field anchor (one entry, the anchor itself, resolved by
/// `import_path_text`'s span-reconstruction fallback below).
pub(super) fn import_entries(anchor: Node) -> Vec<Node> {
    let mut found = Vec::new();
    collect_path_field_nodes(anchor, BOUNDED_SEARCH_DEPTH, &mut found);
    if found.is_empty() {
        vec![anchor]
    } else {
        found
    }
}

fn collect_path_field_nodes<'t>(node: Node<'t>, depth: u8, out: &mut Vec<Node<'t>>) {
    if depth == 0 {
        return;
    }
    if PATH_FIELD_CANDIDATES
        .iter()
        .any(|f| node.child_by_field_name(f).is_some())
    {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_path_field_nodes(child, depth - 1, out);
    }
}

/// Extracts one entry's import path. Tries a clean single-occurrence
/// `PATH_FIELD_CANDIDATES` field first (module doc item 2); when none exists
/// exactly once (Scala's repeated `path` field, or a fieldless grammar
/// entirely), falls back to `span_reconstruct`, which reconstructs the path
/// from the RAW SOURCE SLICE spanning the entry's own non-alias named
/// children — correct by construction because tree-sitter keeps the
/// keyword/modifier tokens as SEPARATE anonymous children before that span,
/// so the slice naturally excludes them while keeping the interior
/// punctuation (dots) the source already contains.
pub(super) fn import_path_text(source: &str, entry: Node) -> Option<String> {
    for field in PATH_FIELD_CANDIDATES {
        let mut cursor = entry.walk();
        let occurrences: Vec<Node> = entry.children_by_field_name(field, &mut cursor).collect();
        if occurrences.len() == 1 {
            let candidate = strip_quotes(&resolve_field_text(source, occurrences[0]));
            if !candidate.is_empty() {
                return Some(candidate);
            }
        }
    }
    span_reconstruct(source, entry)
}

/// A single-occurrence `PATH_FIELD_CANDIDATES` field can itself resolve to a
/// PATH+ALIAS WRAPPER node rather than the bare path (verified: Rust's
/// `use_as_clause`, the `argument` field's target for `use std::io as io2`,
/// itself carries a nested `path` field alongside its `alias` field). One
/// unwrap: when the field's target node has BOTH a `path` field AND an
/// `alias` field, the real path is the NESTED `path` field's text, not the
/// wrapper's whole raw text (which would otherwise fold in `" as io2"`).
/// Deliberately gated on "has both fields", not "has a `path` field" alone —
/// Rust's own `scoped_identifier` also carries `path`+`name` fields for its
/// OWN internal segment-scoping, and unwrapping THAT would truncate the
/// dotted path to its last segment; requiring `alias` too targets exactly
/// the combining-wrapper shape, never the plain dotted-path node.
fn resolve_field_text(source: &str, node: Node) -> String {
    if let (Some(inner_path), true) = (
        node.child_by_field_name("path"),
        node.child_by_field_name("alias").is_some(),
    ) {
        return text(source, inner_path);
    }
    text(source, node)
}

/// See `import_path_text`'s doc. Excludes everything from the entry's own
/// alias onward from the span so an aliased import's alias text is not
/// folded into the path — the alias may sit directly on `entry` (Dart's
/// `import_specification`) or nested one level inside `entry`'s single
/// content child (Python's `aliased_import`, reached via `entry`'s `name`
/// field), so the search is the same bounded one `import_alias_text` uses,
/// not a direct-field-only check.
/// The alias VALUE node's `start_byte` is not by itself a correct upper
/// bound for the path span: the AS_KEYWORD anonymous token that introduces
/// it (`" as "`) sits BETWEEN the path content and the alias, so bounding at
/// the alias node alone would leave the keyword itself in the slice.
/// Widens the boundary to the `as` token's own start when one is found as a
/// sibling of the alias node preceding it.
fn alias_boundary(alias_node: Node) -> usize {
    let Some(parent) = alias_node.parent() else {
        return alias_node.start_byte();
    };
    let mut cursor = parent.walk();
    let mut boundary = alias_node.start_byte();
    for c in parent.children(&mut cursor) {
        if !c.is_named() && c.kind() == AS_KEYWORD && c.start_byte() < boundary {
            boundary = c.start_byte();
        }
    }
    boundary
}

fn span_reconstruct(source: &str, entry: Node) -> Option<String> {
    let alias_node = find_alias_node(entry, BOUNDED_SEARCH_DEPTH);
    let alias_id = alias_node.map(|n| n.id());
    let mut cursor = entry.walk();
    let content: Vec<Node> = entry
        .named_children(&mut cursor)
        .filter(|c| Some(c.id()) != alias_id)
        .collect();
    let first = content.first()?;
    let end_byte = match alias_node {
        Some(a) => alias_boundary(a),
        None => entry.end_byte(),
    };
    if end_byte <= first.start_byte() {
        return None;
    }
    let raw = &source[first.start_byte()..end_byte];
    let trimmed = raw.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
    let candidate = strip_quotes(trimmed);
    if candidate.is_empty() {
        None
    } else {
        Some(candidate)
    }
}

/// Extracts one entry's alias, in order: (a) Go's shape — `path`+`name`
/// fields BOTH present on the same node means `name` is the LOCAL ALIAS, not
/// a second path (module doc item 2's disambiguation); (b) a bounded search
/// for the first `ALIAS_FIELD_CANDIDATES` field anywhere in the entry's
/// subtree (Rust's `use_as_clause` sits one level below the `use_declaration`
/// entry, Python's `aliased_import` one level below `import_statement`); (c)
/// the `AS_KEYWORD`-followed-by-identifier fallback for grammars with no
/// alias field at all.
pub(super) fn import_alias_text(source: &str, entry: Node) -> Option<String> {
    if entry.child_by_field_name("path").is_some() {
        if let Some(name_node) = entry.child_by_field_name("name") {
            let candidate = text(source, name_node);
            if !candidate.is_empty() {
                return Some(candidate);
            }
        }
    }
    if let Some(n) = find_alias_node(entry, BOUNDED_SEARCH_DEPTH) {
        let candidate = text(source, n);
        if !candidate.is_empty() {
            return Some(candidate);
        }
    }
    find_as_token_alias(source, entry, BOUNDED_SEARCH_DEPTH)
}

/// Bounded search for the first `ALIAS_FIELD_CANDIDATES` field anywhere in
/// `node`'s subtree, shared by `import_alias_text` (wants the alias's TEXT)
/// and `span_reconstruct` (wants the alias's POSITION, to bound the path
/// span before it — module doc item 3, and `span_reconstruct`'s own doc for
/// why this must be recursive, not a direct-field-only check).
fn find_alias_node(node: Node, depth: u8) -> Option<Node> {
    if depth == 0 {
        return None;
    }
    for field in ALIAS_FIELD_CANDIDATES {
        if let Some(n) = node.child_by_field_name(field) {
            return Some(n);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_alias_node(child, depth - 1) {
            return Some(found);
        }
    }
    None
}

fn find_as_token_alias(source: &str, node: Node, depth: u8) -> Option<String> {
    if depth == 0 {
        return None;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for pair in children.windows(2) {
        if !pair[0].is_named() && pair[0].kind() == AS_KEYWORD && pair[1].is_named() {
            let candidate = text(source, pair[1]);
            if !candidate.is_empty() {
                return Some(candidate);
            }
        }
    }
    for child in children.iter().filter(|c| c.is_named()) {
        if let Some(found) = find_as_token_alias(source, *child, depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Ruby's/Lua's `require`-family call-as-import (module doc item 5): given a
/// `call` node already classified by TIER 1 as a `CallSite`, extracts the
/// path from the first string-shaped leaf under its `arguments` field —
/// verified: Ruby's `argument_list` wraps a `string` node, Lua's `arguments`
/// wraps a `string` node directly; both grammars name the kind literally
/// `"string"`, so a kind-CONTAINS-"string" scan is exact on both without a
/// per-language string-node-kind table.
pub(super) fn import_call_path(source: &str, call_node: Node) -> Option<String> {
    let args = call_node.child_by_field_name("arguments")?;
    find_string_like_leaf(source, args, BOUNDED_SEARCH_DEPTH)
}

fn find_string_like_leaf(source: &str, node: Node, depth: u8) -> Option<String> {
    if depth == 0 {
        return None;
    }
    if node.is_named() && node.kind().contains("string") {
        let candidate = strip_quotes(&text(source, node));
        if !candidate.is_empty() {
            return Some(candidate);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_string_like_leaf(source, child, depth - 1) {
            return Some(found);
        }
    }
    None
}
