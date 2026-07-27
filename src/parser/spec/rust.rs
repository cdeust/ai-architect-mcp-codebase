// parser::spec::rust — the Rust LangSpec row + RustConventions (ADR-0055 phase 8).
//
// Rust is the language AP is itself written in and carried the largest
// hand-written walker (`src/parser/rust/`, 1649 LOC across 6 files). Its
// extraction routes through the dedicated `rust` walker (see `walkers/rust.rs`),
// so this row carries a `RustFamilySpec` (`rust_family: Some(&RUST_FAMILY)`) and
// leaves the class-model slices empty — the C (#109) / C++ (#125) precedent.
//
// The behavioral escape hatch (ADR-0055 §4) holds the four genuinely behavioral
// Rust divergences:
//   1. **Visibility is a node, not a name.** `pub` / `pub(crate)` / `pub(super)`
//      is a `visibility_modifier` child whose verbatim text IS the visibility;
//      its absence is private (an empty string, not `"private"`).
//   2. **Two import edge kinds.** A `use` declaration's leaves are file-local
//      `Defines`; an `extern crate` is an `Imports` edge to the crate name.
//   3. **The `use` tree expands to atomic leaves.** `use a::{b, c::{d, e}}` is
//      three `Import`s, `use x::y as z` is named `z`, `use x::*` is named
//      `x::*`, and `use std::io::{self, BufRead}` yields `std::io` itself — so
//      q9 (imports in file) and q14 (unresolved externals) can match individual
//      symbols instead of a raw brace substring.
//   4. **A call node can yield several call sites.** The call itself, plus one
//      per function-value argument (issue #87), and a `macro_invocation` yields
//      a call site whose callee carries a trailing `!` so the resolver's Layer 4
//      can tell macros from functions cheaply.
//
// Every node-kind string and field name traces to tree-sitter-rust 0.23.3's
// node-types.json (pinned in Cargo.lock); the spec-validation guard
// (`guard::every_spec_node_kind_is_real_for_its_grammar`) asserts it.
// source: tree-sitter-rust 0.23.3 src/node-types.json
// (github.com/tree-sitter/tree-sitter-rust).

use tree_sitter::Node;

use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::lang_spec::{LangSpec, RustFamilySpec};
use crate::parser::{node_field_text, node_text, qual, Language};

/// One expanded leaf of a `use` tree: the full `::`-joined path, the alias (empty
/// when unaliased), and whether the leaf is a glob.
type UseLeaf = (String, String, bool);

/// Rust behavioral conventions — the ADR Risk-1 watch surface for phase 8.
pub(super) struct RustConventions;

impl RustConventions {
    /// The verbatim text of the node's `visibility_modifier` child, or an empty
    /// string when absent (private). Rust access is a modifier on the item, and
    /// its *exact form* is load-bearing downstream (`pub` vs `pub(crate)` vs
    /// `pub(super)` are different reachability classes), so it is preserved
    /// verbatim rather than collapsed to public/private.
    /// source: Rust Reference §Visibility and Privacy.
    fn visibility_modifier(source: &str, node: Node) -> String {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == RUST_FAMILY.visibility_kind {
                return node_text(source, child);
            }
        }
        String::new()
    }

    /// Expands one node of a `use` tree into its leaves, prepending `prefix`
    /// (with `::`) to each path. `prefix` is what carries the scope across nested
    /// brace lists (`use a::{b, c::{d, e}}`).
    fn use_leaves(source: &str, node: Node, prefix: &str) -> Vec<UseLeaf> {
        let rf = &RUST_FAMILY;
        let k = node.kind();
        if k == rf.use_list_kind {
            Self::use_list_leaves(source, node, prefix)
        } else if k == rf.scoped_use_list_kind {
            let head = node
                .child_by_field_name(rf.path_field)
                .map(|n| node_text(source, n))
                .unwrap_or_default();
            let inner_prefix = join_path(prefix, &head);
            match node.child_by_field_name(rf.list_field) {
                Some(list) => Self::use_list_leaves(source, list, &inner_prefix),
                None => Vec::new(),
            }
        } else if k == rf.use_as_clause_kind {
            let path = node
                .child_by_field_name(rf.path_field)
                .map(|n| node_text(source, n))
                .unwrap_or_default();
            let alias = node
                .child_by_field_name(rf.alias_field)
                .map(|n| node_text(source, n))
                .unwrap_or_default();
            vec![(join_path(prefix, &path), alias, false)]
        } else if k == rf.use_wildcard_kind {
            // The wildcard's text is `<path>::*`, or a bare `*` when nested in a
            // brace list that already carries the prefix. Strip the trailing
            // `::*` (or `*`, then a dangling `::`) and attach what remains to the
            // prefix.
            let text = node_text(source, node);
            let stripped = text.trim_end_matches("::*").trim_end_matches('*');
            vec![(
                join_path(prefix, stripped.trim_end_matches("::")),
                String::new(),
                true,
            )]
        } else {
            // `identifier`, `scoped_identifier`, `crate`, `self`, `super`, … .
            // Inside a brace list, `self` names the prefix itself:
            // `use std::io::{self, BufRead}` imports `std::io` AND
            // `std::io::BufRead`.
            let leaf = node_text(source, node);
            let path = if leaf == "self" && !prefix.is_empty() {
                prefix.to_string()
            } else {
                join_path(prefix, &leaf)
            };
            vec![(path, String::new(), false)]
        }
    }

    /// Expands every NAMED child of a brace list (skipping the `{`, `,`, `}`
    /// punctuation) under `prefix`.
    fn use_list_leaves(source: &str, list: Node, prefix: &str) -> Vec<UseLeaf> {
        let mut cursor = list.walk();
        let mut out: Vec<UseLeaf> = Vec::new();
        for child in list.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            out.extend(Self::use_leaves(source, child, prefix));
        }
        out
    }

    /// Shapes one `CallSite` keyed on `span_node`'s source span. Chained calls
    /// share a start byte (`input.trim().to_string()`), so the (start, end) byte
    /// span — not the start alone — is what makes the id unique among a caller's
    /// call sites. The column is 0-based, as the pre-migration walker emitted it.
    fn call_site(callee: &str, span_node: Node, caller_qn: &str) -> CallEntry {
        let line = span_node.start_position().row as u64 + 1;
        let col = span_node.start_position().column as u64;
        let start_byte = span_node.start_byte() as u64;
        let end_byte = span_node.end_byte() as u64;
        let qn = format!("{caller_qn}::call@{line}:{col}#{start_byte}-{end_byte}");
        CallEntry {
            name: callee.to_string(),
            qualified_name: qn.clone(),
            visibility: String::new(),
            properties: vec![
                ("callee_name".to_string(), callee.to_string()),
                ("caller_qn".to_string(), caller_qn.to_string()),
            ],
            start_line: line,
            end_line: line,
            ref_kind: "Defines",
            ref_to: qn,
        }
    }
}

impl LanguageConventions for RustConventions {
    fn visibility_of(&self, _name: &str) -> String {
        // Rust visibility is node-based (see `node_visibility`), so this
        // name-only entry is never reached for Rust: every Rust emitter computes
        // visibility through `node_visibility`, and `constant_visibility` — the
        // only other caller — fires solely for `value_decl_node_kinds`, which
        // RUST_SPEC leaves empty.
        // mutation note (§12): a mutant of this return value is EQUIVALENT for
        // Rust — no fixture can reach it (§9: no test for an unreachable path).
        String::new()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // Rust methods scope by their `impl`/`trait` block (threaded by the
        // `rust` walker), not by a receiver field; `method_node_kinds` is empty,
        // so `emit_method_recv` — the only caller — never fires for Rust.
        // mutation note (§12): EQUIVALENT for Rust (unreachable path, §9).
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, _seq: u64) -> String {
        // A Rust path is already unique within a file: two items cannot share a
        // name in one module, and an `impl` method is scoped by its receiver
        // type. So no `#seq` suffix and no collision dedup — matching the
        // hand-written walker's plain `qual(scope, name)`.
        qual(scope, name)
    }

    fn node_visibility(&self, source: &str, node: Node, _name: &str) -> String {
        Self::visibility_modifier(source, node)
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        let rf = &RUST_FAMILY;
        // A macro invocation's callee is its macro path plus a `!` marker; a call
        // expression's is its whole callee text, method receivers and paths
        // included (`obj.method`, `helpers::normalize`). The resolver decides
        // what can be bound — an early filter here is what once dropped every
        // method call (Spike B' BUG #10).
        let text = if call_node.kind() == rf.macro_invocation_kind {
            let name = node_field_text(source, call_node, rf.macro_field);
            if name.is_empty() {
                return None;
            }
            format!("{name}!")
        } else {
            node_field_text(source, call_node, rf.callee_field)
        };
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    fn call_entry(
        &self,
        _source: &str,
        call_node: Node,
        caller_qn: &str,
        callee: &str,
        _seq: u64,
    ) -> CallEntry {
        Self::call_site(callee, call_node, caller_qn)
    }

    fn extra_call_entries(&self, source: &str, call_node: Node, caller_qn: &str) -> Vec<CallEntry> {
        // A function passed *by value* (`queue.iter().map(process_order)`) is a
        // real reference to that function, but the argument identifier is not
        // itself a call node, so the DFS never emits a call site for it and the
        // resolver never records the `Calls` edge. Emit one per bare
        // `identifier` / `scoped_identifier` argument. Speculative by design: the
        // resolver drops the reference when the name is a local binding rather
        // than a known function or type, exactly as it drops any other
        // unresolved callee (`len`, `HashMap::new`, …).
        // source: issue #87 gap 3 (rs-D2) — the #64 head-to-head eval showed
        //   worker.rs::drain, which passes `process_order` to `.map`, absent from
        //   the callers of `process_order` (recall 0.5 vs the Grep baseline).
        //
        // No node-kind guard is needed to keep this off macro invocations: a
        // `macro_invocation` has no `arguments` field in the grammar (its payload
        // is a `token_tree`), so the lookup below yields `None` for one. A guard
        // would be unkillable code (§9).
        let rf = &RUST_FAMILY;
        let args = match call_node.child_by_field_name(rf.arguments_field) {
            Some(a) => a,
            None => return Vec::new(),
        };
        let mut cursor = args.walk();
        let mut out = Vec::new();
        for arg in args.children(&mut cursor) {
            if !rf.fn_value_arg_kinds.contains(&arg.kind()) {
                continue;
            }
            let callee = node_text(source, arg);
            if callee.is_empty() {
                continue;
            }
            out.push(Self::call_site(&callee, arg, caller_qn));
        }
        out
    }

    fn import_ref_kind(&self, import_stmt: Node) -> &'static str {
        // `extern crate foo;` names an external crate — a real `Imports` edge to
        // that crate. A `use` leaf declares a name inside THIS file, so its edge
        // is a file-local `Defines` (the resolver upgrades it to a cross-file
        // edge in a later pass).
        if RUST_FAMILY.extern_crate_kinds.contains(&import_stmt.kind()) {
            "Imports"
        } else {
            "Defines"
        }
    }

    fn imports_of(
        &self,
        source: &str,
        _spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        let rf = &RUST_FAMILY;
        let visibility = Self::visibility_modifier(source, import_stmt);
        let start_line = import_stmt.start_position().row as u64 + 1;
        let end_line = import_stmt.end_position().row as u64 + 1;
        if rf.extern_crate_kinds.contains(&import_stmt.kind()) {
            let name = node_field_text(source, import_stmt, RUST_SPEC.name_field);
            if name.is_empty() {
                return Vec::new();
            }
            return vec![ImportEntry {
                display_name: name.clone(),
                qualified_name: qual(scope, &format!("import:{name}")),
                ref_to: name.clone(),
                properties: vec![("path".to_string(), name)],
                visibility,
                start_line,
                end_line,
            }];
        }
        let arg = match import_stmt.child_by_field_name(rf.argument_field) {
            Some(a) => a,
            None => return Vec::new(),
        };
        Self::use_leaves(source, arg, "")
            .into_iter()
            .filter(|(path, _, _)| !path.is_empty())
            .map(|(path, alias, is_glob)| {
                // The display name is the alias when aliased, `path::*` for a
                // glob, else the path itself. It is also the QN suffix, so two
                // leaves of one statement never collide.
                let display_name = if !alias.is_empty() {
                    alias.clone()
                } else if is_glob {
                    format!("{path}::*")
                } else {
                    path.clone()
                };
                let qn = qual(scope, &display_name);
                ImportEntry {
                    display_name,
                    ref_to: qn.clone(),
                    qualified_name: qn,
                    properties: vec![
                        ("path".to_string(), path),
                        ("alias".to_string(), alias),
                        ("is_glob".to_string(), is_glob.to_string()),
                    ],
                    visibility: visibility.clone(),
                    start_line,
                    end_line,
                }
            })
            .collect()
    }
}

/// Joins a `use`-tree prefix and a tail with `::`, tolerating either being empty.
fn join_path(prefix: &str, tail: &str) -> String {
    if prefix.is_empty() {
        tail.to_string()
    } else if tail.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}::{tail}")
    }
}

static RUST_CONVENTIONS: RustConventions = RustConventions;

/// The Rust structural sub-table (ADR-0055 phase 8). All node kinds and field
/// names: tree-sitter-rust 0.23.3 node-types.json (validated by the spec guard).
static RUST_FAMILY: RustFamilySpec = RustFamilySpec {
    attribute_kinds: &["attribute_item"],
    function_kinds: &["function_item"],
    function_signature_kinds: &["function_signature_item"],
    struct_like_kinds: &["struct_item", "union_item"],
    enum_kinds: &["enum_item"],
    variant_kinds: &["enum_variant"],
    variant_list_kinds: &["enum_variant_list"],
    trait_kinds: &["trait_item"],
    impl_kinds: &["impl_item"],
    decl_list_kinds: &["declaration_list"],
    constant_kinds: &["const_item", "static_item"],
    macro_def_kinds: &["macro_definition"],
    extern_crate_kinds: &["extern_crate_declaration"],
    type_alias_kinds: &["type_item"],
    use_kinds: &["use_declaration"],
    mod_kinds: &["mod_item"],
    use_list_kind: "use_list",
    scoped_use_list_kind: "scoped_use_list",
    use_as_clause_kind: "use_as_clause",
    use_wildcard_kind: "use_wildcard",
    macro_invocation_kind: "macro_invocation",
    fn_value_arg_kinds: &["identifier", "scoped_identifier"],
    visibility_kind: "visibility_modifier",
    function_modifiers_kind: "function_modifiers",
    async_kind: "async",
    argument_field: "argument",
    path_field: "path",
    alias_field: "alias",
    list_field: "list",
    trait_field: "trait",
    arguments_field: "arguments",
    macro_field: "macro",
    callee_field: "function",
};

/// The Rust language spec row. The class-model slices are empty — the dedicated
/// `rust` walker consumes `rust_family` — with three deliberate exceptions that
/// feed SHARED walkers rather than the Rust one:
///   - `call_node_kinds` drives the shared `calls::walk_calls` DFS, and carries
///     BOTH Rust call-site kinds (`call_expression` and `macro_invocation`);
///     `RustConventions::call_callee` branches on the kind.
///   - `extends_field` + `base_node_kinds` drive the shared
///     `types::collect_bases`, which is exactly the supertrait rule
///     (`trait T: A + B` → the `bounds` field's type-identifier children).
///   - `field_container_kinds` + `field_node_kinds` name the struct/union field
///     shape the Rust walker's field emitter reads.
///
/// All node-kind strings and field names: tree-sitter-rust 0.23.3
/// node-types.json (validated by `guard::every_spec_node_kind_is_real_for_its_grammar`).
pub(crate) static RUST_SPEC: LangSpec = LangSpec {
    language: Language::Rust,
    skip_node_kinds: &[],
    function_node_kinds: &[],
    method_node_kinds: &[],
    class_node_kinds: &[],
    interface_node_kinds: &[],
    enum_node_kinds: &[],
    variant_node_kinds: &[],
    member_constant_kinds: &[],
    decorated_def_kinds: &[],
    decorator_node_kind: None,
    base_node_kinds: &["type_identifier", "scoped_type_identifier"],
    type_decl_node_kinds: &[],
    type_spec_node_kinds: &[],
    struct_type_kind: None,
    interface_type_kind: None,
    field_container_kinds: &["field_declaration_list"],
    field_node_kinds: &["field_declaration"],
    variable_field_kinds: &[],
    body_wrapper_kinds: &[],
    class_body_kinds: &[],
    function_body_kinds: &[],
    value_decl_node_kinds: &[],
    value_spec_node_kinds: &[],
    // Unused by the Rust walker (Rust consts route through `rust_family`'s
    // `constant_kinds`); a real kind so the guard's unconditional
    // `value_name_kind` check passes.
    value_name_kind: "identifier",
    variable_declarator_kind: None,
    // Empty: Rust's two import statement kinds live in `rust_family`
    // (`use_kinds` / `extern_crate_kinds`) because the walker must tell them
    // apart to pick the edge kind, and one shared slice cannot carry that.
    import_node_kinds: &[],
    import_spec_kinds: &[],
    call_node_kinds: &["call_expression", "macro_invocation"],
    // `OrderConfig { .. }` (issue #92); its `name` field names the constructed type.
    type_construction_kinds: &["struct_expression"],
    name_field: "name",
    body_field: Some("body"),
    type_field: "type",
    receiver_field: None,
    import_path_field: None,
    extends_field: Some("bounds"),
    value_name_field: None,
    value_type_field: None,
    // `fn f() -> OrderConfig` — `return_type` is the type after `->` (issue #92).
    return_type_field: Some("return_type"),
    construction_type_field: Some("name"),
    ts_language: || tree_sitter_rust::LANGUAGE.into(),
    embedded: &[],
    conventions: &RUST_CONVENTIONS,
    c_family: None,
    cpp_family: None,
    objc_family: None,
    ts_family: None,
    ts_language_by_ext: None,
    rust_family: Some(&RUST_FAMILY),
};
