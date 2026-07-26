// parser::spec::go — the Go LangSpec row + GoConventions (ADR-0055 first
// migration). This is the whole of Go's extraction now: a data row plus two
// grammar-specific predicates. Everything else is the generic walkers.
//
// Every node-kind string below is a real kind in tree-sitter-go's
// node-types.json (the executable spec-validation guard asserts it).
// source: tree-sitter-go 0.23.4 src/node-types.json
// (github.com/tree-sitter/tree-sitter-go, pinned in Cargo.lock).

use tree_sitter::Node;

use super::conventions::{ImportEntry, LanguageConventions};
use super::lang_spec::LangSpec;
use crate::parser::{node_field_text, Language};

/// Go behavioral conventions. Only the two genuinely Go-specific predicates —
/// export-by-capitalization visibility and `(recv *T)` receiver parsing —
/// diverge from the generic defaults; QN shaping and call/import extraction
/// are Go's structural conventions, seeded here for the first migration.
pub(super) struct GoConventions;

// Field names in tree-sitter-go used by the conventions (not by the generic
// walkers). source: tree-sitter-go 0.23.4 node-types.json.
const GO_CALL_FUNCTION_FIELD: &str = "function";

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

    fn call_site_qn(&self, caller: &str, line: usize, col: usize, seq: u64) -> String {
        format!("{caller}::call@{line}:{col}#{seq}")
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

    fn import_entry(
        &self,
        source: &str,
        spec: &LangSpec,
        import_spec_node: Node,
        scope: &str,
    ) -> Option<ImportEntry> {
        let path = node_field_text(source, import_spec_node, spec.import_path_field);
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
        })
    }
}

static GO_CONVENTIONS: GoConventions = GoConventions;

/// The Go language spec row. All node-kind strings: tree-sitter-go 0.23.4
/// node-types.json (validated by `tests/spec_node_kinds.rs`).
pub(crate) static GO_SPEC: LangSpec = LangSpec {
    language: Language::Go,
    skip_node_kinds: &["package_clause"],
    function_node_kinds: &["function_declaration"],
    method_node_kinds: &["method_declaration"],
    type_decl_node_kinds: &["type_declaration"],
    type_spec_node_kinds: &["type_spec", "type_alias"],
    struct_type_kind: Some("struct_type"),
    interface_type_kind: Some("interface_type"),
    field_container_kinds: &["field_declaration_list"],
    field_node_kinds: &["field_declaration"],
    value_decl_node_kinds: &["const_declaration", "var_declaration"],
    value_spec_node_kinds: &["const_spec", "var_spec"],
    value_name_kind: "identifier",
    import_node_kinds: &["import_declaration"],
    import_spec_kinds: &["import_spec"],
    call_node_kinds: &["call_expression"],
    name_field: "name",
    body_field: "body",
    type_field: "type",
    receiver_field: "receiver",
    import_path_field: "path",
    ts_language: || tree_sitter_go::LANGUAGE.into(),
    embedded: &[],
    conventions: &GO_CONVENTIONS,
};
