// parser::spec::structural — generic grammar-INTROSPECTION extraction
// (issue #224, owner pivot 2026-08-08: no per-language artifact of any kind —
// no `tags.scm`, no `.tsg`, no per-language Rust module, no external binary).
//
// The mechanism: a compiled `tree_sitter::Language` describes its own node
// kinds and field names at runtime (`node_kind_count`/`node_kind_for_id`,
// `field_count`/`field_name_for_id` — verified against
// `tree-sitter-0.26.11/binding_rust/lib.rs`). Tree-sitter field NAMES are
// conventionalized across grammar authors far more than node-kind names are
// (`function_declaration` in Go vs `function_item` in Rust vs
// `function_definition` in Python — no shared name — but all three expose a
// `name` + `body` + `parameters` field triple on their function-like node).
// So this engine classifies a node by the SET OF FIELDS it structurally
// carries, probed at each real parsed instance via
// `Node::child_by_field_name`, never by matching on its kind string. Onboard
// a new language by adding its grammar crate as a dependency — nothing else.
// That claim is the thing `structural_coverage_tests` exists to verify.
//
// ## The classification table (global, verified against ten real grammars'
// `node-types.json` — see the coverage-table comment in
// `structural_coverage_tests` for the language-by-language evidence)
//
//   name + body + (parameters | params)  -> function-like definition
//   name + body, no parameters/params    -> type-like definition (class/
//                                           struct/module — this engine
//                                           cannot further distinguish class
//                                           vs interface vs module by field
//                                           shape alone; see the coverage
//                                           table's "type granularity" row)
//   function + arguments                 -> call site, callee = `function`
//   method + arguments                   -> call site, callee = `method`
//                                           (Ruby-shaped: receiver optional)
//   name + arguments, no function/method -> call site, callee = `name`
//                                           (Java's `method_invocation`;
//                                           `arguments` here also matches
//                                           Bash's singular `argument` field
//                                           — issue #224's Bash follow-up,
//                                           see `ARGUMENTS_FIELD_CANDIDATES`)
//   method + receiver, no arguments      -> call site, callee = `method`
//                                           (Objective-C's message send,
//                                           which has no `arguments` field —
//                                           its keyword-argument parts are
//                                           unnamed children)
//   target + an `arguments`-KIND child,
//   no `arguments`/`argument` FIELD      -> call site, callee = `target`
//                                           (Elixir's `call` node — issue
//                                           #224's Elixir follow-up, see
//                                           `call_callee_field`'s own doc)
//
// A function-like definition is further split into Method vs Function: EITHER
// it carries a `receiver` field itself (Go's method), OR its nearest
// classified-definition ancestor is type-like (structural nesting — the
// generalization of `shallow.rs`'s `in_class` heuristic, now driven by field
// shape instead of a curated `class_node_kinds` list).
//
// A type-like definition additionally checks a SMALL GLOBAL set of heritage
// field candidates (`superclass`, `superclasses`, `interfaces`, `trait`) —
// verified present on Java/Python/ObjC/Rust's real grammars above — and
// emits an `Inherits` edge per non-empty one found. This is still zero
// per-language data: every definition, in every grammar, is probed against
// the SAME fixed candidate list; a language whose grammar uses none of these
// four names simply contributes no `Inherits` edges (an honest absence, not
// a guess), which is exactly what happened structurally to TypeScript and
// C++ in the verification below (their heritage clauses live on a
// child NODE, not a field of the class node itself — a real, reported gap,
// not silently papered over).
//
// ## What this engine does NOT reach (reported, not hidden — see the
// coverage table and the PR body's four-capability verdict)
//
//   - Imports: CLOSED by the issue #224 imports follow-up
//     (`structural_imports.rs`) — the field-shape approach documented above
//     genuinely has no honest single rule for imports (still true, and still
//     why this is a SEPARATE mechanism from the field-shape classifier
//     above), but a lexical one does: every sampled grammar marks its import
//     keyword as an anonymous child token, which `structural_imports.rs`
//     anchors on directly. Elixir's `import`/`alias`/`require`/`use` and
//     Bash's `source` are NOT keyword-anchored at all (they are ordinary
//     calls, per that module's doc item 5) — CLOSED instead by this module's
//     own `target`-field branch (Elixir) and `ARGUMENTS_FIELD_CANDIDATES`
//     widening (Bash) making them classify as calls in the first place, then
//     `IMPORT_CALL_NAMES` (issue #224's Elixir/Zig/Bash follow-up) flagging
//     the classified call as also being an import. Zig's `@import(...)` is
//     CLOSED by this module's fieldless-call fallback
//     (`is_fieldless_call_with_positional_arguments`, `structural_fallback.rs`)
//     feeding the same `IMPORT_CALL_NAMES` mechanism. See that module's doc
//     for the full mechanism and tables.
//   - Visibility: Java's `modifiers` and TypeScript's `accessibility_modifier`
//     are child NODE kinds, not fields, on their owning declaration (probed
//     directly: `method_declaration`'s field set has no `modifiers` entry
//     even though the grammar defines a `modifiers` node kind). CLOSED by
//     TIER 2's `visibility_via_modifier_child` (`structural_fallback.rs`) —
//     a node-KIND heuristic, not a field probe, scanning direct children for
//     a kind containing `"modifier"`.
//   - Swift and Kotlin (kotlin-ng) calls: BOTH grammars' `call_expression`
//     node carries ZERO named fields (verified against their
//     `node-types.json`) — their call syntax is fully positional. CLOSED by
//     TIER 2's positional call fallback (`kind_hints_positional_call` +
//     `first_named_child` in `structural_fallback.rs`).
//   - Kotlin (kotlin-ng) definitions: `function_declaration` exposes only a
//     `name` field (no `body`, no `parameters`); `class_declaration`
//     likewise exposes only `name`. CLOSED by TIER 2's kind-substring
//     fallback classifier (`kind_hints_definition_role` +
//     `has_parameter_like_child` in `structural_fallback.rs`).

use std::collections::{HashMap, HashSet};

use tree_sitter::{Language as TsLanguage, Node};

use super::structural_fallback::{
    first_named_child, has_parameter_like_child, heritage_targets_via_child_hop,
    is_fieldless_call_with_positional_arguments, kind_hints_definition_role,
    kind_hints_positional_call, resolve_name_via_bounded_scan, resolve_name_via_declarator_chain,
    visibility_via_modifier_child, KindRole,
};
use super::structural_imports::{
    has_import_anchor_ancestor, import_alias_text, import_entries, import_path_text,
    is_import_anchor,
};
use super::structural_imports_calls::{import_call_path, IMPORT_CALL_NAMES};
use super::structural_scope::{
    any_field, emit_calls, emit_defs, emit_imports, has_child_of_kind, has_field, last_segment,
    resolve_scopes, rightmost_named_leaf_text, CallEntry, DefEntry, DefRole, ImportEntry,
};
use crate::parser::{
    collect_error_ranges, count_parse_errors, node_text, parse_tree_too_deep, parse_with_timeout,
    Language, ParseResult, MAX_TREE_DEPTH,
};

/// Field names probed to normalize a possibly-multi-part definition-like
/// parameter list. Both spellings are real: `parameters` (Go/Python/Java/
/// Rust/TypeScript/C-family) and `params` (Swift).
const PARAMETER_FIELD_CANDIDATES: [&str; 2] = ["parameters", "params"];

/// Heritage field candidates, verified present on real grammars above:
/// `superclass` (Java, ObjC), `superclasses` (Python), `interfaces` (Java),
/// `trait` (Rust's `impl Trait for Type`). A language whose grammar uses
/// none of these contributes no `Inherits` edges — see the module doc.
const HERITAGE_FIELD_CANDIDATES: [&str; 4] = ["superclass", "superclasses", "interfaces", "trait"];

/// Field names probed for a call's argument list, checked via `any_field`
/// wherever `call_callee_field` requires (or excludes) one. Both spellings
/// are real, verified against real grammars: `arguments` (Go/Java/Python/
/// Rust/TypeScript/C-family/Ruby/Lua) and, issue #224's Bash follow-up,
/// singular `argument` — Bash's `command` node names its (repeatable)
/// argument-list field `argument`, one character off `arguments`, verified
/// via `tree-sitter-bash`'s `node-types.json` (`command: {fields: {name,
/// argument, redirect}}`). Checked as a candidate LIST rather than widened
/// to a substring match so a grammar using an unrelated `argument`-named
/// field for something else (C's `unary_expression.argument`, Python's
/// `not_operator.argument`, OCaml's `application_expression.argument` — all
/// verified via the same `node-types.json` survey) is only swept in when it
/// ALSO satisfies one of `call_callee_field`'s other required fields
/// (`function`/`method`/`name`), never on its own.
const ARGUMENTS_FIELD_CANDIDATES: [&str; 2] = ["arguments", "argument"];

/// A language described by its grammar alone (issue #224's "zero
/// per-language artifact" requirement, made literal: this struct carries no
/// data about the language's syntax, only how to obtain its compiled
/// grammar).
pub(crate) struct StructuralSpec {
    pub language: Language,
    pub ts_language: fn() -> TsLanguage,
}

/// Diagnostic counters proving the engine's honesty about what it could not
/// classify, surfaced to the coverage tests rather than silently dropped.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct StructuralStats {
    /// Named nodes with a `name` field whose grammar-defined kind never
    /// satisfied any classification rule (Kotlin's sparse fields land here).
    pub unclassified_named_nodes_with_a_name_field: u32,
}

/// One classified call site: the callee-bearing field name to read, chosen by
/// which structural signature the node matched. `None` when nothing matched.
///
/// The final branch (issue #224's Elixir follow-up) is structurally
/// different from the other four: Elixir's `call` node (its `import`/
/// `alias`/`require`/`use`/`def` macros are ALL ordinary calls — homoiconic
/// macro system, verified via `node-types.json`) declares only a `target`
/// FIELD; its argument list is a positional CHILD node of kind `arguments`,
/// never a field at all. Gating on `has_child_of_kind(node, "arguments")`
/// rather than a field check is what lets this branch reach Elixir without
/// misfiring on Swift's OWN `target`-field nodes (`assignment`,
/// `navigation_expression`, `postfix_expression`, `prefix_expression`,
/// `check_expression`) — verified via `node-types.json` that none of those
/// five Swift node kinds ever declare an `arguments`-kind child at all, so
/// the gate excludes them with no per-language exception.
fn call_callee_field(node: Node) -> Option<&'static str> {
    if has_field(node, "function") && any_field(node, &ARGUMENTS_FIELD_CANDIDATES) {
        return Some("function");
    }
    if has_field(node, "method") && any_field(node, &ARGUMENTS_FIELD_CANDIDATES) {
        return Some("method");
    }
    if has_field(node, "name")
        && any_field(node, &ARGUMENTS_FIELD_CANDIDATES)
        && !has_field(node, "body")
    {
        return Some("name");
    }
    if has_field(node, "method")
        && has_field(node, "receiver")
        && !any_field(node, &ARGUMENTS_FIELD_CANDIDATES)
    {
        return Some("method");
    }
    if has_field(node, "target") && has_child_of_kind(node, "arguments") {
        return Some("target");
    }
    None
}

/// Resolves function-vs-type once a definition candidate's name is already
/// known, preferring field shape (TIER 1: `parameters`/`params` field) and
/// falling back, in order, to a positional parameter-shaped child (TIER 2a)
/// and finally the node's own kind vocabulary (TIER 2b:
/// `kind_hints_definition_role`). Shared by BOTH the TIER 1 and TIER 2
/// branches of `classify` below: a node can satisfy TIER 1's `name`+`body`
/// field check (Swift's `function_declaration` does — see the module doc)
/// while still needing this refinement, since `parameters`/`params` is the
/// ONE field TIER 1's own name+body gate does not itself require. Splitting
/// "found a definition" from "which kind of definition" this way is what
/// lets Swift be fixed without bypassing TIER 1's field check for it.
fn resolve_def_role(node: Node) -> DefRole {
    if any_field(node, &PARAMETER_FIELD_CANDIDATES) || has_parameter_like_child(node) {
        return DefRole::FunctionLike;
    }
    match kind_hints_definition_role(node.kind()) {
        Some(KindRole::FunctionLike) => DefRole::FunctionLike,
        Some(KindRole::TypeLike) | None => DefRole::TypeLike,
    }
}

/// Classifies one node into a definition role, a call, or neither, in two
/// tiers. TIER 1 reads the node's OWN field shape first — `name`+`body` for
/// a definition candidate (role resolved via `resolve_def_role` above),
/// `function`/`method`/`name`+`arguments` for calls — so every
/// already-full-recall language (Python/Java/TypeScript/Ruby/Go) keeps its
/// exact prior qualified names.
///
/// TIER 2 (`structural_fallback`, issue #224 follow-up) runs only when TIER 1
/// found no definition-or-call SHAPE at all (no `name`+`body`, no callee
/// field), and only on nodes whose OWN kind already looks definition-shaped
/// (`kind_hints_definition_role`) or call-shaped
/// (`kind_hints_positional_call`) — see the module doc there for why that
/// gate is necessary before descending into declarator chains or bounded
/// identifier scans, and why it cannot false-positive on wrapper nodes like
/// `class_body`/`function_value_parameters`/`call_suffix`.
fn classify(node: Node) -> Role {
    let name = has_field(node, "name");
    let body = has_field(node, "body");
    if name && body {
        let role = resolve_def_role(node);
        let name_node = node
            .child_by_field_name("name")
            .expect("name field checked above");
        return Role::Def { role, name_node };
    }
    if let Some(field) = call_callee_field(node) {
        let callee_node = node
            .child_by_field_name(field)
            .expect("field checked above");
        return Role::Call { callee_node };
    }

    if kind_hints_definition_role(node.kind()).is_some() {
        let name_node =
            resolve_name_via_declarator_chain(node).or_else(|| resolve_name_via_bounded_scan(node));
        if let Some(name_node) = name_node {
            return Role::Def {
                role: resolve_def_role(node),
                name_node,
            };
        }
    }
    // Issue #224's Zig follow-up: `kind_hints_positional_call` keys on a
    // kind-name vocabulary (`"call"` substring + `"expression"` suffix),
    // which Zig's `builtin_function` (`@import(...)`) never matches — no
    // stable kind-name convention exists for fieldless builtin/prefix calls
    // the way it does for `call_expression`. `is_fieldless_call_with_positional_arguments`
    // is the structural (not kind-name) equivalent: see its doc in
    // `structural_fallback.rs`.
    if kind_hints_positional_call(node.kind()) || is_fieldless_call_with_positional_arguments(node)
    {
        if let Some(callee_node) = first_named_child(node) {
            return Role::Call { callee_node };
        }
    }

    Role::None { has_name: name }
}

enum Role<'t> {
    Def { role: DefRole, name_node: Node<'t> },
    Call { callee_node: Node<'t> },
    None { has_name: bool },
}

/// Parses `source` with `spec`'s grammar and runs the structural
/// classification above over the whole tree, emitting the shared
/// `ParseResult` contract.
///
/// Preconditions: `spec.ts_language` is a compiled grammar; `file_path` is
/// the file's repo-relative id and the top scope. Postconditions: returns
/// `Ok((ParseResult, StructuralStats))` where `ParseResult.nodes` are
/// `Function`/`Method`/`Struct`/`CallSite` and `ParseResult.refs` are
/// `Defines`/`HasMethod`/`Calls`/`Inherits`, or `Err` on grammar-set or
/// parse-timeout failure — never panics. `StructuralStats` counts
/// unclassifiable named nodes so a caller can measure recall honestly.
pub(crate) fn parse_structural(
    spec: &StructuralSpec,
    source: &str,
    file_path: &str,
) -> Result<(ParseResult, StructuralStats), String> {
    let lang: TsLanguage = (spec.ts_language)();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set {:?} language: {e}", spec.language))?;
    let tree = parse_with_timeout(&mut parser, source)?;
    if parse_tree_too_deep(tree.root_node(), MAX_TREE_DEPTH) {
        return Err(format!(
            "parse_tree_too_deep: exceeds {MAX_TREE_DEPTH} levels (adversarial or generated nesting)"
        ));
    }

    let mut stats = StructuralStats::default();
    let mut defs: Vec<DefEntry> = Vec::new();
    let mut calls: Vec<CallEntry> = Vec::new();
    // Keyed by the DEFINING NODE'S `id()`, never by a `defs` vector index:
    // `defs` is discovered in stack-pop (reverse-child) order and only
    // acquires its FINAL, index-stable order after `defs.sort_by_key`
    // below. A raw `defs.len()`-at-discovery-time index silently goes stale
    // across that sort (root-cause fixed here, not pinned into the coverage
    // tests as a "known gap" — this bug pre-dates TIER 2 and was invisible
    // before because every prior fixture had at most one heritage-bearing
    // definition, so any reordering coincidentally still hit index 0).
    let mut heritage: Vec<(usize, String)> = Vec::new(); // (defining node id, target name)
                                                         // Issue #224 follow-up: import extraction (see `structural_imports.rs`).
                                                         // Collected independently of `classify`'s Def/Call branches below — a
                                                         // node can be an import anchor while classify() returns `Role::None`
                                                         // (Java's `import_declaration` matches no def/call field shape at all),
                                                         // so this is an orthogonal check on the same walk, not a third arm of
                                                         // the match.
    let mut imports: Vec<ImportEntry> = Vec::new();

    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_named() {
            if is_import_anchor(node) && !has_import_anchor_ancestor(node) {
                for entry in import_entries(node) {
                    if let Some(path) = import_path_text(source, entry) {
                        let alias = import_alias_text(source, entry);
                        imports.push(ImportEntry {
                            node: entry,
                            path,
                            alias,
                        });
                    }
                }
            }
            match classify(node) {
                Role::Def { role, name_node } => {
                    let name = node_text(source, name_node);
                    if !name.is_empty() {
                        if role == DefRole::TypeLike {
                            let mut seen_targets: HashSet<String> = HashSet::new();
                            for field in HERITAGE_FIELD_CANDIDATES {
                                if let Some(target_node) = node.child_by_field_name(field) {
                                    let text = rightmost_named_leaf_text(source, target_node);
                                    let target = last_segment(&text);
                                    if !target.is_empty() && seen_targets.insert(target.clone()) {
                                        heritage.push((node.id(), target));
                                    }
                                }
                            }
                            // TIER 2 heritage-on-child-node hop (fixes
                            // TypeScript, C++): a SEPARATE detection path
                            // from the field-based one above (mutually
                            // exclusive per grammar in practice, but both
                            // checked unconditionally; deduped against the
                            // same `seen_targets` set so a grammar that
                            // somehow satisfied both never double-emits).
                            for target_node in heritage_targets_via_child_hop(node) {
                                let target = last_segment(&node_text(source, target_node));
                                if !target.is_empty() && seen_targets.insert(target.clone()) {
                                    heritage.push((node.id(), target));
                                }
                            }
                        }
                        let visibility = visibility_via_modifier_child(source, node);
                        defs.push(DefEntry {
                            node,
                            role,
                            name,
                            is_method: false, // filled by resolve_scopes
                            qn: String::new(),
                            visibility,
                        });
                    }
                }
                Role::Call { callee_node } => {
                    let callee = last_segment(&node_text(source, callee_node));
                    if !callee.is_empty() {
                        // Ruby's/Lua's `require`-family (structural_imports
                        // module doc item 5): these are ordinary `call`
                        // nodes TIER 1 already classifies above, not
                        // keyword-anchored statements — a call whose callee
                        // matches also emits an Import, in addition to its
                        // normal CallSite/Calls edge.
                        if IMPORT_CALL_NAMES.contains(&callee.as_str()) {
                            if let Some(path) = import_call_path(source, node) {
                                imports.push(ImportEntry {
                                    node,
                                    path,
                                    alias: None,
                                });
                            }
                        }
                        calls.push(CallEntry { node, callee });
                    }
                }
                Role::None { has_name } => {
                    if has_name {
                        stats.unclassified_named_nodes_with_a_name_field += 1;
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    defs.sort_by_key(|d| d.node.start_byte());
    resolve_scopes(&mut defs, file_path);

    let node_id_to_def: HashMap<usize, usize> = defs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.node.id(), i))
        .collect();

    // Emission is split across `structural_scope.rs`'s `emit_defs`/
    // `emit_calls`/`emit_imports` purely for the coding-standards.md §4.1
    // 500-line file cap (this function was pushing past it once the
    // Elixir/Zig/Bash follow-up's classification landed) — no new mechanism,
    // only where the ExtractedNode/ExtractedRef shape gets built from the
    // data the walk above already produced.
    let (mut nodes, mut refs) = emit_defs(&defs, &heritage, &node_id_to_def, file_path);
    let (call_nodes, call_refs) = emit_calls(&calls, &node_id_to_def, &defs, file_path);
    nodes.extend(call_nodes);
    refs.extend(call_refs);
    let (import_nodes, import_refs) = emit_imports(&imports, &defs, &node_id_to_def, file_path);
    nodes.extend(import_nodes);
    refs.extend(import_refs);

    Ok((
        ParseResult {
            nodes,
            refs,
            parse_errors: count_parse_errors(tree.root_node()),
            error_ranges: collect_error_ranges(tree.root_node()),
        },
        stats,
    ))
}
