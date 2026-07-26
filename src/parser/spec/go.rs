// parser::spec::go — the Go LangSpec row + GoConventions (ADR-0055 first
// migration, phase 1). This is the whole of Go's extraction now: a data row
// plus a handful of grammar-specific predicates. Everything else is the
// generic walkers.
//
// Every node-kind string below is a real kind in tree-sitter-go's
// node-types.json (the executable spec-validation guard asserts it).
// source: tree-sitter-go 0.23.4 src/node-types.json
// (github.com/tree-sitter/tree-sitter-go, pinned in Cargo.lock).

use tree_sitter::Node;

use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::lang_spec::LangSpec;
use crate::parser::{node_field_text, Language};

/// Go behavioral conventions. Only the genuinely Go-specific predicates —
/// export-by-capitalization visibility, `(recv *T)` receiver parsing, `#seq`
/// QN shaping, and last-segment callee extraction — diverge from the generic
/// defaults.
pub(super) struct GoConventions;

// Field names in tree-sitter-go used by the conventions (not by the generic
// walkers). source: tree-sitter-go 0.23.4 node-types.json.
const GO_CALL_FUNCTION_FIELD: &str = "function";

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
            properties: vec![("callee_name".to_string(), callee.to_string())],
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
    name_field: "name",
    body_field: Some("body"),
    type_field: "type",
    receiver_field: Some("receiver"),
    import_path_field: Some("path"),
    extends_field: None,
    value_name_field: None,
    value_type_field: None,
    ts_language: || tree_sitter_go::LANGUAGE.into(),
    embedded: &[],
    conventions: &GO_CONVENTIONS,
    c_family: None,
};
