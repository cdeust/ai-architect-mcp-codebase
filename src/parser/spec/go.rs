// parser::spec::go — the Go LangSpec row (ADR-0055 first migration, phase 1;
// issue #220 phase 1: migrated to the declarative deep path).
//
// Production now runs Go through `DeclarativeConventions(&GO_RULES)`
// (`declarative.rs`) — a `ConventionSpec` data row plus the shared generic
// interpreter, proving the machinery issue #220 phase 1 introduces. The
// hand-written `GoConventions` below is KEPT, unused by `GO_SPEC`, as the
// rollback path and the parity oracle: `go_parity_tests.rs` asserts the
// declarative path and `GoConventions` produce byte-identical output on the
// same corpus before any deletion is considered (ADR rollback discipline —
// the hand-written struct is never deleted in the same PR that introduces
// its declarative row).
//
// Every node-kind string below is a real kind in tree-sitter-go's
// node-types.json (the executable spec-validation guard asserts it).
// source: tree-sitter-go 0.23.4 src/node-types.json
// (github.com/tree-sitter/tree-sitter-go, pinned in Cargo.lock).

use super::declarative::DeclarativeConventions;
use super::declarative_rules::{
    CallEntryRule, CallSiteQnScheme, CalleeDispatchRow, CalleeTransform, ConventionSpec,
    ImportRule, PropertySet, QnScheme, ReceiverPattern, RefToRule, VisibilityRule,
};
use super::lang_spec::LangSpec;
use crate::parser::Language;

// Used only by the test-only `GoConventions` (parity oracle) below.
#[cfg(test)]
use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
#[cfg(test)]
use crate::parser::node_field_text;
#[cfg(test)]
use tree_sitter::Node;

/// Go's six-method behavior as a `ConventionSpec` data row (issue #220 phase
/// 1). Every value below is read directly off `GoConventions` (this file,
/// preserved below) — this row reproduces it exactly, proven by
/// `go_parity_tests.rs`.
pub(super) static GO_RULES: ConventionSpec = ConventionSpec {
    // GoConventions::visibility_of: uppercase first letter ⇒ "public", else
    // "package".
    visibility: VisibilityRule::NameCase {
        public_label: "public",
        else_label: "package",
    },
    // GoConventions::receiver_type: `(c *T)` → `T` — strip parens, take the
    // last whitespace token (drops the receiver's variable name), strip a
    // leading `*` (pointer receiver).
    receiver_pattern: Some(ReceiverPattern {
        strip_parens: true,
        take_last_whitespace_token: true,
        strip_leading_sigils: &['*'],
    }),
    // GoConventions::def_qn: `{scope}::{name}#{seq}`.
    qn_scheme: QnScheme::SeqSuffixed,
    // GoConventions::call_callee: read the `function` field, take the tail
    // segment after the last `.`, require an identifier-leading result.
    callee_dispatch: &[CalleeDispatchRow {
        node_kind: None,
        field: GO_CALL_FUNCTION_FIELD,
        fallback_field: None,
        transform: CalleeTransform::TailSegment(&['.']),
        require_leading_ident: true,
        suffix: None,
    }],
    // GoConventions::call_entry: `{caller}::call@{line}:{col}#{seq}`,
    // `Calls`, ref_to = the callee verbatim, `public`, `[callee_name]`.
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "public",
        properties: PropertySet::CalleeOnly,
    },
    // GoConventions::imports_of / go_import_entry: DFS the import statement
    // for nodes matching `GO_SPEC.import_spec_kinds` (`import_spec`), each
    // yielding one entry from its `GO_SPEC.import_path_field` (`"path"`).
    import_rule: ImportRule::ContainerTable,
};

static GO_DECLARATIVE_CONVENTIONS: DeclarativeConventions = DeclarativeConventions(&GO_RULES);

/// Go's hand-written behavioral conventions (ADR-0055 phase 1). Superseded in
/// production by `GO_DECLARATIVE_CONVENTIONS` above (issue #220 phase 1).
/// Gated `#[cfg(test)]`: its only caller is the test-only `GO_SPEC_LEGACY`
/// row below, which `go_parity_tests.rs` uses as the parity oracle — see the
/// module doc comment. Kept, not deleted, per the ADR rollback discipline.
#[cfg(test)]
pub(super) struct GoConventions;

// Field name in tree-sitter-go used by both the production callee-dispatch
// row (`GO_RULES` above) and the test-only legacy `GoConventions` below.
// source: tree-sitter-go 0.23.4 node-types.json.
const GO_CALL_FUNCTION_FIELD: &str = "function";

#[cfg(test)]
impl GoConventions {
    /// Shapes one Go `import_spec` node into an `ImportEntry`, or `None` when
    /// the path is empty. Called by `imports_of` for every import spec found
    /// under an `import_declaration`.
    fn go_import_entry(
        &self,
        source: &str,
        spec: &LangSpec,
        import_spec_node: Node,
        scope: &str,
    ) -> Option<ImportEntry> {
        let path_field = spec.import_path_field?;
        let path = node_field_text(source, import_spec_node, path_field);
        let cleaned = path.trim_matches('"').to_string();
        if cleaned.is_empty() {
            return None;
        }
        let display_name = cleaned.rsplit('/').next().unwrap_or(&cleaned).to_string();
        Some(ImportEntry {
            display_name,
            qualified_name: format!("{scope}::import:{cleaned}"),
            ref_to: cleaned.clone(),
            properties: vec![("path".to_string(), cleaned)],
            visibility: "public".to_string(),
            start_line: import_spec_node.start_position().row as u64 + 1,
            end_line: import_spec_node.end_position().row as u64 + 1,
        })
    }
}

#[cfg(test)]
impl LanguageConventions for GoConventions {
    fn visibility_of(&self, name: &str) -> String {
        // Exported iff the first letter is uppercase; idiomatic Go convention.
        match name.chars().next() {
            Some(c) if c.is_uppercase() => "public".to_string(),
            _ => "package".to_string(),
        }
    }

    fn receiver_type(&self, receiver_text: &str) -> String {
        // Strip `(*T)` or `(T)` (optionally named, `(c *T)`) down to `T`.
        receiver_text
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split_whitespace()
            .last()
            .unwrap_or("")
            .trim_start_matches('*')
            .to_string()
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        format!("{scope}::{name}#{seq}")
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        let callee = node_field_text(source, call_node, GO_CALL_FUNCTION_FIELD);
        let tail = callee
            .rsplit('.')
            .next()
            .unwrap_or("")
            .trim_end_matches('(')
            .trim()
            .to_string();
        if !tail.is_empty()
            && tail
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
        {
            Some(tail)
        } else {
            None
        }
    }

    fn call_entry(
        &self,
        _source: &str,
        call_node: Node,
        caller_qn: &str,
        callee: &str,
        seq: u64,
    ) -> CallEntry {
        let line = call_node.start_position().row + 1;
        let col = call_node.start_position().column + 1;
        CallEntry {
            name: callee.to_string(),
            qualified_name: format!("{caller_qn}::call@{line}:{col}#{seq}"),
            visibility: "public".to_string(),
            properties: vec![
                ("callee_name".to_string(), callee.to_string()),
                // source: LSP 3.17 Base Protocol — positions are 0-based;
                // `col` above is 1-based (this spec's QN convention), so
                // `lsp_col` carries the raw tree-sitter 0-based column
                // separately. Read by
                // indexer::persist::nodes::append_label_properties.
                (
                    "lsp_col".to_string(),
                    call_node.start_position().column.to_string(),
                ),
            ],
            start_line: call_node.start_position().row as u64 + 1,
            end_line: call_node.end_position().row as u64 + 1,
            ref_kind: "Calls",
            ref_to: callee.to_string(),
        }
    }

    fn imports_of(
        &self,
        source: &str,
        spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        // Stack DFS over the `import_declaration` for each `import_spec`,
        // matching the pre-migration hand-written walker (single `import "x"`
        // and grouped `import ( ... )`). Order is irrelevant — the output is
        // consumed as a set.
        let mut out = Vec::new();
        let mut stack = vec![import_stmt];
        while let Some(n) = stack.pop() {
            if spec.import_spec_kinds.contains(&n.kind()) {
                if let Some(e) = self.go_import_entry(source, spec, n, scope) {
                    out.push(e);
                }
            }
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                stack.push(c);
            }
        }
        out
    }
}

#[cfg(test)]
static GO_CONVENTIONS: GoConventions = GoConventions;

/// The Go language spec row. All node-kind strings: tree-sitter-go 0.23.4
/// node-types.json (validated by `guard::every_spec_node_kind_is_real_for_its_grammar`).
pub(crate) static GO_SPEC: LangSpec = LangSpec {
    language: Language::Go,
    skip_node_kinds: &["package_clause"],
    function_node_kinds: &["function_declaration"],
    method_node_kinds: &["method_declaration"],
    class_node_kinds: &[],
    interface_node_kinds: &[],
    enum_node_kinds: &[],
    variant_node_kinds: &[],
    member_constant_kinds: &[],
    decorated_def_kinds: &[],
    decorator_node_kind: None,
    base_node_kinds: &[],
    type_decl_node_kinds: &["type_declaration"],
    type_spec_node_kinds: &["type_spec", "type_alias"],
    struct_type_kind: Some("struct_type"),
    interface_type_kind: Some("interface_type"),
    field_container_kinds: &["field_declaration_list"],
    field_node_kinds: &["field_declaration"],
    variable_field_kinds: &[],
    body_wrapper_kinds: &[],
    class_body_kinds: &[],
    function_body_kinds: &[],
    value_decl_node_kinds: &["const_declaration", "var_declaration"],
    value_spec_node_kinds: &["const_spec", "var_spec"],
    value_name_kind: "identifier",
    variable_declarator_kind: None,
    import_node_kinds: &["import_declaration"],
    import_spec_kinds: &["import_spec"],
    call_node_kinds: &["call_expression"],
    // `OrderConfig{..}` (issue #92); its `type` field names the constructed type.
    type_construction_kinds: &["composite_literal"],
    name_field: "name",
    body_field: Some("body"),
    type_field: "type",
    receiver_field: Some("receiver"),
    import_path_field: Some("path"),
    extends_field: None,
    value_name_field: None,
    value_type_field: None,
    // `func F() OrderConfig` — the `result` field is the return-type annotation.
    return_type_field: Some("result"),
    construction_type_field: Some("type"),
    ts_language: || tree_sitter_go::LANGUAGE.into(),
    embedded: &[],
    // issue #220 phase 1: production runs the declarative row, not
    // `GoConventions` — see the module doc comment for the rollback story.
    conventions: &GO_DECLARATIVE_CONVENTIONS,
    c_family: None,
    cpp_family: None,
    objc_family: None,
    ts_family: None,
    ts_language_by_ext: None,
    rust_family: None,
};

/// Test-only twin of `GO_SPEC` wired to the hand-written `GoConventions`
/// instead of the declarative row — the parity oracle `go_parity_tests.rs`
/// parses the same corpus through both and asserts identical output.
/// Every field but `conventions` is copied verbatim from `GO_SPEC` (all
/// `LangSpec` fields are `Copy` types, so `..GO_SPEC` is a plain field copy,
/// not a move out of a `static`).
#[cfg(test)]
pub(crate) static GO_SPEC_LEGACY: LangSpec = LangSpec {
    conventions: &GO_CONVENTIONS,
    ..GO_SPEC
};
