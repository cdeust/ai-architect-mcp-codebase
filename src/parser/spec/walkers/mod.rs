// parser::spec::walkers — the generic tree-sitter walkers (ADR-0055 §1).
//
// `walk_defs` / `walk_calls` / `walk_imports` / `walk_embedded` consume a
// `&LangSpec` (node-kind data) and a `&dyn LanguageConventions` (behavior) and
// produce the EXISTING, unchanged `ParseResult` / `ExtractedNode` /
// `ExtractedRef` contract. One implementation each, replacing the per-language
// hand-written walkers one language at a time behind the accuracy gate.
//
// The walker skeleton spans the structural patterns of the migrated languages,
// each pattern gated by (empty-for-non-applicable) spec data:
//   - Go   uses `method_node_kinds` (receiver-scoped), `type_decl_node_kinds`
//          (struct/interface/alias with fields), and a multi-name DFS constant
//          path. It leaves `class_node_kinds` / `decorated_def_kinds` empty.
//   - Python uses `class_node_kinds` (recurse into the body; in-class functions
//          become methods by enclosing scope), `decorated_def_kinds`, and a
//          field-based constant path. It leaves `method_node_kinds` /
//          `type_decl_node_kinds` empty.
// A node kind belongs to exactly one slice, so the dispatch is unambiguous and
// a language selects the arms it needs by populating (or emptying) its slices —
// OCP: adding a language is data, not a new walker (ADR-0055 §1.2).

mod calls;
mod clike;
mod constants;
mod cpp;
mod cpp_members;
mod declarator;
mod defs;
mod embedded;
mod imports;
mod objc;
mod rust;
mod rust_body;
mod rust_types;
mod type_uses;
mod types;
mod typescript;

pub(crate) use defs::walk_defs;
pub(super) use types::collect_bases;

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use super::lang_spec::LangSpec;
use crate::parser::{
    collect_error_ranges, count_parse_errors, parse_tree_too_deep, parse_with_timeout,
    ExtractedNode, ExtractedRef, ParseResult, MAX_TREE_DEPTH,
};

/// Mutable state threaded through a single file's walk. `next_seq` is the
/// per-file monotonic counter the conventions use to disambiguate overloads
/// (Go's `#seq` suffix) and to key call sites. `emitted_qns` backs the
/// def-QN collision dedup (Python's `@property`/`@setter` case); it is inert
/// for languages whose `def_qn` is already unique (Go's `#seq`).
pub(crate) struct WalkCtx<'a> {
    pub(super) source: &'a str,
    /// The file's repo-relative id — the outermost scope of the walk. Read by
    /// the `rust` walker, whose `impl` methods attach to `{file_path}::{Type}`
    /// regardless of the enclosing module (an `impl` inside `mod helpers` still
    /// scopes its methods to the file), so the scope alone cannot supply it.
    pub(super) file_path: &'a str,
    pub(super) nodes: Vec<ExtractedNode>,
    pub(super) refs: Vec<ExtractedRef>,
    pub(super) next_seq: u64,
    pub(super) emitted_qns: HashSet<String>,
    /// Issue #353. On the first walk of a Rust file that mentions `cfg`, the
    /// effective `#[cfg]` gate of every DEFINITION node, as (index in `nodes`,
    /// gate): call sites and other nodes are pushed by other walkers, so the
    /// index is what lines a gate up with its node. Used to find twins (one label
    /// and qualified name under distinct gates). `None` otherwise, so a file
    /// without `cfg` pays nothing.
    pub(super) cfg_gates: Option<Vec<(usize, String)>>,
    /// Issue #353. The (label, qualified name) pairs that have twins, each with
    /// the gates of its members, set only on the second walk of a file that has
    /// some: the emitters then append each twin's gate to its qualified name.
    /// Empty on every other walk.
    pub(super) twins: HashMap<(String, String), Vec<String>>,
}

impl<'a> WalkCtx<'a> {
    /// A fresh walk of `source`. `cfg_gates` is `Some` on the first walk of a Rust
    /// file that mentions `cfg`; `twins` is non-empty only on the second walk.
    fn new(
        source: &'a str,
        file_path: &'a str,
        cfg_gates: Option<Vec<(usize, String)>>,
        twins: HashMap<(String, String), Vec<String>>,
    ) -> Self {
        WalkCtx {
            source,
            file_path,
            nodes: Vec::new(),
            refs: Vec::new(),
            next_seq: 0,
            emitted_qns: HashSet::new(),
            cfg_gates,
            twins,
        }
    }
}

impl WalkCtx<'_> {
    pub(super) fn next_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    /// Returns a unique QN: the input if unseen, else `qn@{start_line}` so every
    /// definition has a unique primary key while preserving the readable name
    /// for resolver name-based lookups. A no-op for `def_qn`s that are already
    /// unique (Go appends `#seq`), the mechanism for Python overload pairs.
    pub(super) fn dedup(&mut self, qn: String, start_line: u64) -> String {
        if self.emitted_qns.insert(qn.clone()) {
            return qn;
        }
        let unique = format!("{qn}@{start_line}");
        self.emitted_qns.insert(unique.clone());
        unique
    }
}

/// Parses `source` with `spec`'s grammar and extracts the uniform
/// `ParseResult`. This is the table-driven replacement for a language's
/// hand-written `parse_<lang>_file` entry point.
///
/// Preconditions: `spec.ts_language` is the grammar matching `source`'s
/// language; `file_path` is the file's repo-relative id (the top scope).
/// Postconditions: returns `Ok(ParseResult)` whose `nodes`/`refs` are exactly
/// what the generic walkers emit for `source`, plus the shared parse-error
/// signals; `Err` only on grammar-set or parse-timeout failure. Invariant:
/// the public `ExtractedNode`/`ExtractedRef` contract is unchanged.
pub(crate) fn parse_with_spec(
    spec: &LangSpec,
    source: &str,
    file_path: &str,
) -> Result<ParseResult, String> {
    parse_with_spec_twins(spec, source, file_path, true)
}

/// `parse_with_spec` with the #353 twin identity switched off: the walk that
/// existed before it. Only the identity test uses it, to compare the two over
/// every Rust file of the repository.
#[cfg(test)]
pub(crate) fn parse_with_spec_no_twins(
    spec: &LangSpec,
    source: &str,
    file_path: &str,
) -> Result<ParseResult, String> {
    parse_with_spec_twins(spec, source, file_path, false)
}

fn parse_with_spec_twins(
    spec: &LangSpec,
    source: &str,
    file_path: &str,
    twin_identity: bool,
) -> Result<ParseResult, String> {
    // Most languages have one fixed grammar (`ts_language`); TypeScript selects
    // between its `typescript` and `tsx` grammars by file extension
    // (`ts_language_by_ext`), because JSX syntax is only in the tsx grammar.
    let lang: tree_sitter::Language = match spec.ts_language_by_ext {
        Some(select) => select(file_path),
        None => (spec.ts_language)(),
    };
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set {:?} language: {e}", spec.language))?;
    let tree = parse_with_timeout(&mut parser, source)?;
    // Depth guard (issue #148): the definition walkers recurse one frame per tree
    // level, so a pathologically deep error-recovery tree would overflow the
    // stack or exhaust the heap. Reject before walking — a clean `Err` the
    // indexer maps to a Skipped file.
    if parse_tree_too_deep(tree.root_node(), MAX_TREE_DEPTH) {
        return Err(format!(
            "parse_tree_too_deep: exceeds {MAX_TREE_DEPTH} levels (adversarial or generated nesting)"
        ));
    }

    let track_gates = twin_identity && spec.rust_family.is_some() && source.contains("cfg");
    let mut ctx = WalkCtx::new(
        source,
        file_path,
        track_gates.then(Vec::new),
        HashMap::new(),
    );
    walk_defs(spec, &mut ctx, tree.root_node(), file_path, None);
    // Issue #353: a second walk only when the first found twins, so a file
    // without twins produces exactly the output it always did.
    if let Some(twins) = twin_keys(&ctx) {
        ctx = WalkCtx::new(source, file_path, None, twins);
        walk_defs(spec, &mut ctx, tree.root_node(), file_path, None);
    }
    Ok(ParseResult {
        nodes: ctx.nodes,
        refs: ctx.refs,
        parse_errors: count_parse_errors(tree.root_node()),
        error_ranges: collect_error_ranges(tree.root_node()),
    })
}

/// The (label, qualified name) pairs of `ctx`'s nodes that more than one item
/// emitted under DISTINCT gates, or `None` when there are none. Duplicates under
/// the SAME gate are not twins: they keep today's behaviour (the persistence
/// layer keeps the first).
fn twin_keys(ctx: &WalkCtx) -> Option<HashMap<(String, String), Vec<String>>> {
    let gates = ctx.cfg_gates.as_ref()?;
    let mut seen: HashMap<(&str, &str), HashSet<&str>> = HashMap::new();
    for (index, gate) in gates {
        let node = ctx.nodes.get(*index)?;
        seen.entry((node.label.as_str(), node.qualified_name.as_str()))
            .or_default()
            .insert(gate.as_str());
    }
    let twins: HashMap<(String, String), Vec<String>> = seen
        .into_iter()
        .filter(|(_, gates)| gates.len() > 1)
        .map(|((label, qn), gates)| {
            let mut gates: Vec<String> = gates.into_iter().map(str::to_string).collect();
            gates.sort_unstable();
            ((label.to_string(), qn.to_string()), gates)
        })
        .collect();
    (!twins.is_empty()).then_some(twins)
}

pub(super) fn kind_in(kinds: &[&str], k: &str) -> bool {
    kinds.contains(&k)
}

/// First direct child of `node` whose kind is in `kinds`, or `None`. The
/// cursor temporary is bound before the return so it drops before the cursor
/// (E0597), matching the hand-written parsers' `child_of_kind` helper.
fn first_child_in<'t>(node: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| kind_in(kinds, c.kind()));
    found
}

/// The class-like body to recurse into: the first `class_body_kinds` child when
/// the grammar exposes bodies as child nodes (Kotlin), else the `body_field`
/// child (Go/Python/Java). `None` when neither is present (bodiless class).
pub(super) fn class_body_of<'t>(spec: &LangSpec, node: Node<'t>) -> Option<Node<'t>> {
    if !spec.class_body_kinds.is_empty() {
        return first_child_in(node, spec.class_body_kinds);
    }
    spec.body_field.and_then(|f| node.child_by_field_name(f))
}

/// The node to scan for calls in a function/method, selected by whether the
/// grammar names a body field:
///   - `body_field` set (Go/Python/Java/Swift): use the named body child; if the
///     grammar omits it on this node (Swift `subscript_declaration`, which has no
///     `body` field), fall back to the first `function_body_kinds` child (Swift
///     `computed_property`) — and to nothing when neither is present (a bodiless
///     abstract method), so no unrelated sub-expression is scanned.
///   - `body_field` unset (Kotlin): the body IS a child node
///     (`function_body_kinds`); an expression-bodied `fun f() = g()` has no such
///     child, so the whole declaration node is the fallback scan target.
///
/// This split keeps the risky whole-node fallback confined to grammars with no
/// named body field (Kotlin), where it is the only way to reach an expression
/// body; a named-field grammar (Swift) never scans a whole declaration, so a
/// call inside a parameter default value is not spuriously collected.
pub(super) fn call_scan_of<'t>(spec: &LangSpec, node: Node<'t>) -> Option<Node<'t>> {
    if let Some(field) = spec.body_field {
        if let Some(body) = node.child_by_field_name(field) {
            return Some(body);
        }
        return first_child_in(node, spec.function_body_kinds);
    }
    if !spec.function_body_kinds.is_empty() {
        return first_child_in(node, spec.function_body_kinds).or(Some(node));
    }
    None
}

pub(super) fn line_of(node: Node) -> u64 {
    node.start_position().row as u64 + 1
}

pub(super) fn end_line_of(node: Node) -> u64 {
    node.end_position().row as u64 + 1
}
