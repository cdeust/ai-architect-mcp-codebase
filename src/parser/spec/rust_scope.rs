// parser::spec::rust_scope — which names are BOUND at a call site, so the
// speculative by-value argument scan (issue #87) can tell a value from a
// function reference.
// source: ADR-9836, measured on 2026-09-09 against DYResearch/dy-wcet @ 1e93ccd.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::parser::node_text;

/// The node kind that bounds a binding scope for this analysis. A closure
/// does NOT: its body sees every name its enclosing function binds, so
/// stopping there hid those bindings from a call inside the closure (issue
/// #329, measured on DYResearch/dy-wcet v4.1.2 `examples/rta_probe.rs:85`).
/// source: tree-sitter-rust 0.24.2 src/node-types.json.
const FUNCTION_KIND: &str = "function_item";

/// A closure, the scope root only when no function encloses it (a closure
/// in a `const`/`static` initializer).
const CLOSURE_KIND: &str = "closure_expression";

/// Node kinds that introduce bindings through a `pattern` field.
/// source: tree-sitter-rust 0.24.2 src/node-types.json (`parameter` and
/// `let_declaration` both declare a required `pattern` field).
const BINDING_KINDS: [&str; 2] = ["parameter", "let_declaration"];

/// Node kinds that bind names through a `pattern` field in addition to
/// `BINDING_KINDS`: `if let` / `while let` (`let_condition`), a `match` arm and
/// a `for` loop. Counted only where a name must be bound EXACTLY ONCE
/// (`typed_local_map`, `once_bound_bindings`), so that a name rebound by one of
/// them cannot keep the type of an earlier binding. `bound_names_in_scope`
/// keeps its narrower set: adding names there would change which by-value
/// arguments count as function references.
/// source: tree-sitter-rust 0.24.2 src/node-types.json (`let_condition`,
/// `match_arm` and `for_expression` each declare a `pattern` field).
const EXTRA_BINDING_KINDS: [&str; 3] = ["let_condition", "match_arm", "for_expression"];

/// A closure's parameter list. Its children are `parameter` (typed, `|x: T|`,
/// already a `BINDING_KINDS` node) or a bare `_pattern` (untyped, `|x|`),
/// which has no `pattern` field because it IS the pattern.
/// source: tree-sitter-rust 0.24.2 src/node-types.json (`closure_parameters`
/// children: `_pattern` | `parameter`).
const CLOSURE_PARAMETERS_KIND: &str = "closure_parameters";

/// The `pattern` field name shared by both binding kinds.
const PATTERN_FIELD: &str = "pattern";

/// Leaf kinds that NAME a binding inside a pattern. A struct pattern's
/// shorthand field (`Point { x, y }`) is its own kind, not an `identifier`,
/// so collecting only the latter silently misses it.
/// source: tree-sitter-rust 0.24.2 src/node-types.json.
const BINDING_LEAF_KINDS: [&str; 2] = ["identifier", "shorthand_field_identifier"];

/// Every name bound by the function enclosing `call_node`: its parameters,
/// its `let` declarations, and those of every closure inside it.
///
/// precondition: `call_node` is a node inside a parsed Rust tree.
/// postcondition: the returned set contains only identifier texts read from
/// `source`; an empty set when `call_node` sits outside any function or
/// closure. Scope is approximated at function granularity by design; see
/// ADR-9836.
pub(super) fn bound_names_in_scope(source: &str, call_node: Node) -> HashSet<String> {
    let mut names = HashSet::new();
    if let Some(scope) = enclosing_scope(call_node) {
        for (_, pattern) in binding_patterns(scope, false) {
            collect_identifiers(source, pattern, &mut names);
        }
    }
    names
}

/// True when a `use` declaration inside the function that encloses `node`
/// mentions any `::` segment of `path` as a whole word. The index records the
/// `use` items of files and modules but not those of function bodies, so such
/// a declaration may rebind the name in a way the resolver cannot see.
/// source: measured on tree-sitter-rust 0.24.2 through `parse_file` (issue #339).
pub(super) fn scope_use_mentions(source: &str, node: Node, path: &str) -> bool {
    let Some(scope) = enclosing_scope(node) else {
        return false;
    };
    let segments: Vec<&str> = path.split("::").collect();
    let mut stack = vec![scope];
    while let Some(current) = stack.pop() {
        if current.kind() == "use_declaration" {
            let text = node_text(source, current);
            let mentioned = text
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|word| segments.contains(&word));
            if mentioned {
                return true;
            }
            continue;
        }
        let mut cursor = current.walk();
        stack.extend(current.children(&mut cursor));
    }
    false
}

/// The nearest enclosing `function_item`, `call_node` itself included; when
/// none encloses it, the outermost enclosing closure.
fn enclosing_scope(call_node: Node) -> Option<Node> {
    let mut outermost_closure = None;
    let mut node = Some(call_node);
    while let Some(current) = node {
        match current.kind() {
            FUNCTION_KIND => return Some(current),
            CLOSURE_KIND => outermost_closure = Some(current),
            _ => {}
        }
        node = current.parent();
    }
    outermost_closure
}

/// Every `(declaring node, pattern)` pair under `scope`: a `parameter` or
/// `let_declaration` with its `pattern` field, and each untyped closure
/// parameter, which is its own declaring node and pattern.
fn binding_patterns(scope: Node, every_form: bool) -> Vec<(Node, Node)> {
    let mut out = Vec::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if BINDING_KINDS.contains(&node.kind())
            || (every_form && EXTRA_BINDING_KINDS.contains(&node.kind()))
        {
            if let Some(pattern) = node.child_by_field_name(PATTERN_FIELD) {
                out.push((node, pattern));
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if node.kind() == CLOSURE_PARAMETERS_KIND && !BINDING_KINDS.contains(&child.kind()) {
                out.push((child, child));
            }
            stack.push(child);
        }
    }
    out
}

/// Harvests every `identifier` leaf inside one pattern, so destructuring
/// binds all of its names rather than only the simple case.
pub(super) fn collect_identifiers(source: &str, pattern: Node, out: &mut HashSet<String>) {
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
// traversal: reusing `enclosing_scope`/`binding_patterns`/
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

/// Every name bound EXACTLY ONCE in the function enclosing `call_node`
/// (closures included, issue #329), by a plain (optionally `mut`) identifier pattern, mapped to
/// its simplified type (generics stripped, reduced to the last `::`
/// segment) — the receiver-hint-eligible subset of `bound_names_in_scope`'s
/// broader name set.
///
/// precondition: `call_node` is a node inside a parsed Rust tree.
/// postcondition: `name` is a key iff it is bound EXACTLY ONCE anywhere in
/// the enclosing scope — by a `parameter`/`let_declaration`/untyped closure
/// parameter of ANY pattern shape, simple or destructured (a second binding
/// under a destructuring pattern, or a closure parameter or `let` inside a
/// closure that shadows an outer name, still counts and still disqualifies)
/// — AND that one binding is
/// itself a plain identifier pattern with a type derivable from one of the
/// three plan §2.2 palier-3 forms: a typed parameter, a typed `let`, or a
/// `let x = T::assoc(...)` constructor call. A name bound more than once,
/// bound only by a destructuring pattern, or bound with an untypable
/// initializer is simply absent from the map — never a `None` value, so
/// callers can use plain `.get(name)`. With each type, for a type read off
/// `Type::assoc(..)`, the name of `assoc`: the resolver checks what `assoc`
/// returns (issue #370).
pub(super) fn typed_local_declared(source: &str, call_node: Node) -> HashMap<String, Declared> {
    typed_local_map(source, call_node, false)
}

fn types_only(map: HashMap<String, Declared>) -> HashMap<String, String> {
    map.into_iter().map(|(name, d)| (name, d.ty)).collect()
}

/// `typed_local_bindings` with each type as written, path included
/// (`fmt::Formatter`, `std::fs::File`) instead of its last segment. The macro
/// pass needs the qualifier to tell a std type from a namesake (issue #339).
pub(super) fn typed_local_paths(source: &str, call_node: Node) -> HashMap<String, String> {
    types_only(typed_local_map(source, call_node, true))
}

fn typed_local_map(source: &str, call_node: Node, full_path: bool) -> HashMap<String, Declared> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut typed: HashMap<String, Declared> = HashMap::new();
    let Some(scope) = enclosing_scope(call_node) else {
        return HashMap::new();
    };
    let all = binding_patterns(scope, true);
    for &(node, pattern) in &all {
        let mut names = HashSet::new();
        collect_identifiers(source, pattern, &mut names);
        for name in &names {
            *counts.entry(name.clone()).or_insert(0) += 1;
        }
        let reaches = node.kind() != "let_declaration"
            || super::rust_item_binds::declaration_reaches(node, call_node);
        if let Some(simple_name) = simple_identifier_name(source, pattern).filter(|_| reaches) {
            if let Some(ty) = binding_declared(source, node, full_path) {
                typed.insert(simple_name, ty);
            }
        }
    }
    let rebound = other_binders(source, scope);
    let mut result: HashMap<String, Declared> = counts
        .into_iter()
        .filter(|(name, n)| *n == 1 && !rebound.contains(name))
        .filter_map(|(name, _)| typed.remove(&name).map(|ty| (name, ty)))
        .collect();
    for live in super::rust_live_binding::live_bindings(source, call_node, &all, &rebound) {
        let simple = simple_identifier_name(source, live.pattern);
        if simple.as_deref() != Some(live.name.as_str()) {
            continue;
        }
        if let Some(ty) = binding_declared(source, live.declaration, full_path) {
            result.insert(live.name, ty);
        }
    }
    result
}

/// Names bound by something other than a pattern: an item of the function
/// (`const`, `static`, const generic, `use`) or a macro that may bind it.
fn other_binders(source: &str, scope: Node) -> HashSet<String> {
    let mut names = super::rust_macro_binds::names_macros_may_rebind(source, scope);
    names.extend(super::rust_item_binds::names_items_bind(source, scope));
    names
}

/// One name bound exactly once in a scope, with the node that declares it and
/// the pattern that names it.
pub(super) struct OnceBound<'t> {
    pub(super) name: String,
    pub(super) declaration: Node<'t>,
    pub(super) pattern: Node<'t>,
}

/// Every name bound EXACTLY ONCE in the function enclosing `call_node`, under
/// any pattern shape, with its declaring node and pattern. The same count
/// `typed_local_map` uses, so the two can never disagree about "bound once"
/// (issues #348 and #349 read the initialiser, which `typed_local_map` does
/// not keep).
pub(super) fn once_bound_bindings<'t>(source: &str, call_node: Node<'t>) -> Vec<OnceBound<'t>> {
    let Some(scope) = enclosing_scope(call_node) else {
        return Vec::new();
    };
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut sites: Vec<(String, Node<'t>, Node<'t>)> = Vec::new();
    let all = binding_patterns(scope, true);
    for &(declaration, pattern) in &all {
        let mut names = HashSet::new();
        collect_identifiers(source, pattern, &mut names);
        for name in names {
            *counts.entry(name.clone()).or_insert(0) += 1;
            sites.push((name, declaration, pattern));
        }
    }
    let rebound = other_binders(source, scope);
    let mut once: Vec<OnceBound<'t>> = sites
        .into_iter()
        .filter(|(name, _, _)| counts.get(name) == Some(&1) && !rebound.contains(name))
        .map(|(name, declaration, pattern)| OnceBound {
            name,
            declaration,
            pattern,
        })
        .collect();
    once.extend(super::rust_live_binding::live_bindings(
        source, call_node, &all, &rebound,
    ));
    once
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

/// A binding's declared type and, when the type was read off `Type::assoc(..)`,
/// the name of `assoc` (issue #370): the written path says which type `assoc`
/// belongs to, not what it returns.
pub(super) struct Declared {
    pub(super) ty: String,
    pub(super) assoc: Option<String>,
}

/// The simplified type ONE `parameter`/`let_declaration` binding declares or
/// constructs, per plan §2.2 palier 3's three concrete forms:
///   1. `x: [&][mut] T` (parameter's required `type` field, or a `let`'s
///      optional one) — `type_name` strips the reference and any
///      generic-parameter list.
///   2. `let x = T::assoc(...)` — no `type` field; the initializer's callee
///      must be a `scoped_identifier` (`T::assoc`, or `mod::T::assoc`), and
///      the type is that path's own last segment.
///
/// A bare `let x = make();` (callee has no `::`) or any other initializer
/// shape (`let x = 5;`, `let x = other_call();` with a non-scoped callee)
/// returns `None` — "un initialiseur non typable", plan §2.2.
fn binding_declared(source: &str, binding_node: Node, full_path: bool) -> Option<Declared> {
    if let Some(ty) = binding_node.child_by_field_name(TYPE_FIELD) {
        let ty = type_name(source, ty, full_path)?;
        return Some(Declared { ty, assoc: None });
    }
    if binding_node.kind() != "let_declaration" {
        return None;
    }
    let value = binding_node.child_by_field_name(VALUE_FIELD)?;
    let (call, unwrapped) = constructor_call(source, value, full_path)?;
    let func = call.child_by_field_name(FUNCTION_FIELD)?;
    if func.kind() != "scoped_identifier" {
        return None;
    }
    if unwrapped && !is_known_constructor(source, func) {
        return None;
    }
    let path = func.child_by_field_name(PATH_FIELD)?;
    let assoc = func
        .child_by_field_name(NAME_FIELD)
        .map(|n| node_text(source, n));
    let ty = expr_path_name(source, path, full_path)?;
    Some(Declared { ty, assoc })
}

// source: std constructors that return `Self`, or `Result<Self, _>` through
// `io::Result`: `File::create`, `File::open`, `BufWriter::new`,
// `Vec::with_capacity`, `TcpStream::connect` (https://doc.rust-lang.org/std/).
const KNOWN_CONSTRUCTORS: [&str; 6] = [
    "new",
    "create",
    "create_new",
    "open",
    "with_capacity",
    "connect",
];
// source: `Result::unwrap` and `Result::expect` return the `Ok` value, the
// same type `?` yields.
const RESULT_UNWRAPPERS: [&str; 2] = ["unwrap", "expect"];

/// The call a `let` initialiser is. With `through_results` (the macro
/// destination lookup, issue #339) it looks through `?`, `.unwrap()` and
/// `.expect(..)` and reports that it did; any other wrapper, `x.map(..)` for
/// one, ends the search.
pub(super) fn constructor_call<'t>(
    source: &str,
    value: Node<'t>,
    through_results: bool,
) -> Option<(Node<'t>, bool)> {
    let mut node = value;
    let mut unwrapped = false;
    loop {
        let inner = match node.kind() {
            "call_expression" if through_results => unwrapped_receiver(source, node),
            "try_expression" if through_results => Some(node.named_child(0)?),
            "call_expression" => return Some((node, unwrapped)),
            _ => return None,
        };
        match inner {
            Some(next) => {
                node = next;
                unwrapped = true;
            }
            None => return Some((node, unwrapped)),
        }
    }
}

/// `x` of `x.unwrap()` or `x.expect(..)`.
fn unwrapped_receiver<'t>(source: &str, call: Node<'t>) -> Option<Node<'t>> {
    let function = call.child_by_field_name(FUNCTION_FIELD)?;
    if function.kind() != "field_expression" {
        return None;
    }
    let field = node_text(source, function.child_by_field_name("field")?);
    if !RESULT_UNWRAPPERS.contains(&field.as_str()) {
        return None;
    }
    function.child_by_field_name(VALUE_FIELD)
}

/// True when the associated function of `T::assoc` is a known constructor.
fn is_known_constructor(source: &str, scoped: Node) -> bool {
    scoped
        .child_by_field_name(NAME_FIELD)
        .is_some_and(|n| KNOWN_CONSTRUCTORS.contains(&node_text(source, n).as_str()))
}

/// A TYPE expression's last segment with generics stripped: unwraps
/// `reference_type` (`&T`, `&mut T`) and `generic_type` (`Wrapper<T>` ->
/// `Wrapper`) recursively, then reads a `type_identifier` verbatim or a
/// `scoped_type_identifier`'s own `name` field (`mod::Type` -> `Type`).
/// Anything else (tuple types, slice/array types, `dyn Trait`, primitive
/// types, ...) is not a plain named type this rule covers: `None`.
fn type_name(source: &str, node: Node, full_path: bool) -> Option<String> {
    match node.kind() {
        "reference_type" | "generic_type" => node
            .child_by_field_name(TYPE_FIELD)
            .and_then(|n| type_name(source, n, full_path)),
        "type_identifier" => Some(node_text(source, node)),
        "scoped_type_identifier" if full_path => Some(node_text(source, node)),
        "scoped_type_identifier" => node
            .child_by_field_name(NAME_FIELD)
            .map(|n| node_text(source, n)),
        _ => None,
    }
}

/// An EXPRESSION path's last segment (mirrors `type_name` for the
/// `T::assoc(...)` constructor-call form, whose callee is parsed as an
/// expression path, not a type path): a bare `identifier` (`T::assoc`'s
/// path IS `T`) verbatim, a `scoped_identifier`'s own `name` field
/// (`mod::T::assoc`'s path is itself `mod::T`, whose last segment is `T`),
/// or a `generic_type` path's `type` field recursively (turbofish-shaped
/// paths, rare in this position). Anything else: `None`.
fn expr_path_name(source: &str, node: Node, full_path: bool) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node_text(source, node)),
        "scoped_identifier" if full_path => Some(node_text(source, node)),
        "scoped_identifier" => node
            .child_by_field_name(NAME_FIELD)
            .map(|n| node_text(source, n)),
        "generic_type" => node
            .child_by_field_name(TYPE_FIELD)
            .and_then(|n| expr_path_name(source, n, full_path)),
        _ => None,
    }
}
