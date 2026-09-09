// parser::spec::rust_scope — which names are BOUND at a call site, so the
// speculative by-value argument scan (issue #87) can tell a value from a
// function reference.
// source: ADR-9836, measured on 2026-09-09 against DYResearch/dy-wcet @ 1e93ccd.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::parser::node_text;

/// Node kinds that open a new binding scope for this analysis.
/// source: tree-sitter-rust 0.24.2 src/node-types.json.
const SCOPE_KINDS: [&str; 2] = ["function_item", "closure_expression"];

/// Node kinds that introduce bindings through a `pattern` field.
/// source: tree-sitter-rust 0.24.2 src/node-types.json (`parameter` and
/// `let_declaration` both declare a required `pattern` field).
const BINDING_KINDS: [&str; 2] = ["parameter", "let_declaration"];

/// The `pattern` field name shared by both binding kinds.
const PATTERN_FIELD: &str = "pattern";

/// Leaf kinds that NAME a binding inside a pattern. A struct pattern's
/// shorthand field (`Point { x, y }`) is its own kind, not an `identifier`,
/// so collecting only the latter silently misses it.
/// source: tree-sitter-rust 0.24.2 src/node-types.json.
const BINDING_LEAF_KINDS: [&str; 2] = ["identifier", "shorthand_field_identifier"];

/// Every name bound by the function or closure enclosing `call_node`: its
/// parameters and its `let` declarations.
///
/// precondition: `call_node` is a node inside a parsed Rust tree.
/// postcondition: the returned set contains only identifier texts read from
/// `source`; an empty set when `call_node` sits outside any function.
/// Scope is approximated at function granularity by design; see ADR-9836.
pub(super) fn bound_names_in_scope(source: &str, call_node: Node) -> HashSet<String> {
    let mut names = HashSet::new();
    let Some(scope) = enclosing_scope(call_node) else {
        return names;
    };
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if BINDING_KINDS.contains(&node.kind()) {
            if let Some(pattern) = node.child_by_field_name(PATTERN_FIELD) {
                collect_identifiers(source, pattern, &mut names);
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    names
}

/// The nearest ancestor that opens a binding scope, `call_node` itself
/// included.
fn enclosing_scope(call_node: Node) -> Option<Node> {
    let mut node = call_node;
    loop {
        if SCOPE_KINDS.contains(&node.kind()) {
            return Some(node);
        }
        node = node.parent()?;
    }
}

/// Harvests every `identifier` leaf inside one pattern, so destructuring
/// binds all of its names rather than only the simple case.
fn collect_identifiers(source: &str, pattern: Node, out: &mut HashSet<String>) {
    let mut stack = vec![pattern];
    while let Some(node) = stack.pop() {
        if BINDING_LEAF_KINDS.contains(&node.kind()) {
            let text = node_text(source, node);
            if !text.is_empty() {
                out.insert(text);
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}

// ---------------------------------------------------------------------------
// Typed local bindings — issue #283 palier 3 (lot 6), ADR-<pending>.
//
// `bound_names_in_scope` (above) answers "is this name bound at all" for
// issue #87's by-value-argument scan; `receiver_hint` (issue #283 palier 3)
// needs a strictly stronger answer — "is this name bound EXACTLY ONCE, and
// if so by a plain identifier pattern carrying a determinable type" — so it
// is a second, richer read of the SAME binding walk rather than a second
// traversal: reusing `enclosing_scope`/`BINDING_KINDS`/`PATTERN_FIELD`/
// `collect_identifiers` verbatim is what keeps the two passes from ever
// disagreeing about what "bound" means (ADR-9836's own rationale).
// ---------------------------------------------------------------------------

/// The `type` field name shared by `parameter` (required) and
/// `let_declaration` (optional). source: tree-sitter-rust 0.24.2
/// src/node-types.json.
const TYPE_FIELD: &str = "type";
/// `let_declaration`'s optional initializer field.
const VALUE_FIELD: &str = "value";
/// `call_expression`'s callee field (`T::assoc` in `let s = T::assoc(...)`).
const FUNCTION_FIELD: &str = "function";
/// The prefix field shared by `scoped_identifier` (expression paths) and
/// `scoped_type_identifier` (type paths) — everything before the path's
/// final `::` segment.
const PATH_FIELD: &str = "path";
/// The final-segment field shared by the same two node kinds.
const NAME_FIELD: &str = "name";

/// Every name bound EXACTLY ONCE in the function/closure enclosing
/// `call_node`, by a plain (optionally `mut`) identifier pattern, mapped to
/// its simplified type (generics stripped, reduced to the last `::`
/// segment) — the receiver-hint-eligible subset of `bound_names_in_scope`'s
/// broader name set.
///
/// precondition: `call_node` is a node inside a parsed Rust tree.
/// postcondition: `name` is a key iff it is bound EXACTLY ONCE anywhere in
/// the enclosing scope — by a `parameter`/`let_declaration` of ANY pattern
/// shape, simple or destructured (a second binding under a destructuring
/// pattern still counts and still disqualifies) — AND that one binding is
/// itself a plain identifier pattern with a type derivable from one of the
/// three plan §2.2 palier-3 forms: a typed parameter, a typed `let`, or a
/// `let x = T::assoc(...)` constructor call. A name bound more than once,
/// bound only by a destructuring pattern, or bound with an untypable
/// initializer is simply absent from the map — never a `None` value, so
/// callers can use plain `.get(name)`.
pub(super) fn typed_local_bindings(source: &str, call_node: Node) -> HashMap<String, String> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut typed: HashMap<String, String> = HashMap::new();
    let Some(scope) = enclosing_scope(call_node) else {
        return HashMap::new();
    };
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if BINDING_KINDS.contains(&node.kind()) {
            if let Some(pattern) = node.child_by_field_name(PATTERN_FIELD) {
                let mut names = HashSet::new();
                collect_identifiers(source, pattern, &mut names);
                for name in &names {
                    *counts.entry(name.clone()).or_insert(0) += 1;
                }
                if let Some(simple_name) = simple_identifier_name(source, pattern) {
                    if let Some(ty) = binding_declared_type(source, node) {
                        typed.insert(simple_name, ty);
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n == 1)
        .filter_map(|(name, _)| typed.remove(&name).map(|ty| (name, ty)))
        .collect()
}

/// The name bound by a pattern that is a plain identifier, optionally
/// wrapped in `mut` (`mut_pattern`) — the only two pattern shapes plan §2.2
/// palier 3 considers "a simple identifier". Any other pattern kind
/// (destructuring, tuple, struct, reference, ...) returns `None`: those
/// names are still counted by the caller's `collect_identifiers` pass (so
/// they correctly invalidate a same-named simple binding elsewhere), just
/// never carry a type from THIS binding site.
fn simple_identifier_name(source: &str, pattern: Node) -> Option<String> {
    match pattern.kind() {
        "identifier" => Some(node_text(source, pattern)),
        "mut_pattern" => {
            let mut cursor = pattern.walk();
            let found = pattern
                .named_children(&mut cursor)
                .find(|c| c.kind() == "identifier");
            found.map(|n| node_text(source, n))
        }
        _ => None,
    }
}

/// The simplified type ONE `parameter`/`let_declaration` binding declares or
/// constructs, per plan §2.2 palier 3's three concrete forms:
///   1. `x: [&][mut] T` (parameter's required `type` field, or a `let`'s
///      optional one) — `type_last_segment` strips the reference and any
///      generic-parameter list.
///   2. `let x = T::assoc(...)` — no `type` field; the initializer's callee
///      must be a `scoped_identifier` (`T::assoc`, or `mod::T::assoc`), and
///      the type is that path's own last segment.
///
/// A bare `let x = make();` (callee has no `::`) or any other initializer
/// shape (`let x = 5;`, `let x = other_call();` with a non-scoped callee)
/// returns `None` — "un initialiseur non typable", plan §2.2.
fn binding_declared_type(source: &str, binding_node: Node) -> Option<String> {
    if let Some(ty) = binding_node.child_by_field_name(TYPE_FIELD) {
        return type_last_segment(source, ty);
    }
    if binding_node.kind() != "let_declaration" {
        return None;
    }
    let value = binding_node.child_by_field_name(VALUE_FIELD)?;
    if value.kind() != "call_expression" {
        return None;
    }
    let func = value.child_by_field_name(FUNCTION_FIELD)?;
    if func.kind() != "scoped_identifier" {
        return None;
    }
    let path = func.child_by_field_name(PATH_FIELD)?;
    expr_path_last_segment(source, path)
}

/// A TYPE expression's last segment with generics stripped: unwraps
/// `reference_type` (`&T`, `&mut T`) and `generic_type` (`Wrapper<T>` ->
/// `Wrapper`) recursively, then reads a `type_identifier` verbatim or a
/// `scoped_type_identifier`'s own `name` field (`mod::Type` -> `Type`).
/// Anything else (tuple types, slice/array types, `dyn Trait`, primitive
/// types, ...) is not a plain named type this rule covers: `None`.
fn type_last_segment(source: &str, node: Node) -> Option<String> {
    match node.kind() {
        "reference_type" | "generic_type" => node
            .child_by_field_name(TYPE_FIELD)
            .and_then(|n| type_last_segment(source, n)),
        "type_identifier" => Some(node_text(source, node)),
        "scoped_type_identifier" => node
            .child_by_field_name(NAME_FIELD)
            .map(|n| node_text(source, n)),
        _ => None,
    }
}

/// An EXPRESSION path's last segment (mirrors `type_last_segment` for the
/// `T::assoc(...)` constructor-call form, whose callee is parsed as an
/// expression path, not a type path): a bare `identifier` (`T::assoc`'s
/// path IS `T`) verbatim, a `scoped_identifier`'s own `name` field
/// (`mod::T::assoc`'s path is itself `mod::T`, whose last segment is `T`),
/// or a `generic_type` path's `type` field recursively (turbofish-shaped
/// paths, rare in this position). Anything else: `None`.
fn expr_path_last_segment(source: &str, node: Node) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node_text(source, node)),
        "scoped_identifier" => node
            .child_by_field_name(NAME_FIELD)
            .map(|n| node_text(source, n)),
        "generic_type" => node
            .child_by_field_name(TYPE_FIELD)
            .and_then(|n| expr_path_last_segment(source, n)),
        _ => None,
    }
}
