// parser::spec::c — the C LangSpec row + CConventions (ADR-0055 phase 6).
//
// C is the first FLAT C-family language: its extraction routes through the
// `clike` walker (see `walkers/clike.rs`) rather than the class-model arms, so
// this row carries a `CFamilySpec` (`c_family: Some(&C_FAMILY)`) and leaves the
// class-model slices empty. The behavioral escape hatch (ADR-0055 §4) holds the
// three genuinely C-specific predicates: `#include` interpretation, the
// member-access-aware call callee (`a->b` / `a.b` → `b`), and the `#{seq}`
// function QN. Visibility is uniformly `public` (C has no access keyword).
//
// Every node-kind string traces to tree-sitter-c 0.23.4's node-types.json
// (github.com/tree-sitter/tree-sitter-c, pinned in Cargo.lock); the spec-
// validation guard (`guard::every_spec_node_kind_is_real_for_its_grammar`)
// asserts it.
// source: tree-sitter-c 0.23.4 src/node-types.json.

use tree_sitter::Node;

use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::lang_spec::{CFamilySpec, LangSpec};
use crate::parser::{node_field_text, node_text, qual, Language};

/// The tree-sitter-c field naming a call expression's callee. Used only by the
/// conventions (not the generic walkers), so it is a local const rather than a
/// spec field — mirroring `go.rs`'s `GO_CALL_FUNCTION_FIELD`.
/// source: tree-sitter-c 0.23.4 node-types.json (call_expression.function).
const C_CALL_FUNCTION_FIELD: &str = "function";

/// C behavioral conventions. C has no visibility keyword (everything is
/// `public`) and no receiver concept; the only real behavior is `#include`
/// shaping, member-access callee extraction, and the `#{seq}` QN — the rest are
/// trivial trait obligations.
pub(super) struct CConventions;

impl LanguageConventions for CConventions {
    fn visibility_of(&self, _name: &str) -> String {
        // C exposes no access-control keyword on a top-level declaration; the
        // hand-written walker hardcoded "public" on every node.
        "public".to_string()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // C has no methods / receivers; never reached (no `method_node_kinds`,
        // and the flat `clike` walker has no receiver path).
        // mutation note (§12): mutants of this body SURVIVE and are EQUIVALENT
        // for C — the flat walker never calls `receiver_type`, so no observable
        // output depends on it. It is a required trait obligation, not a live
        // C code path; the C++/ObjC migrations that reuse `CConventions`-style
        // rows will exercise it if they model receivers. Not a coverage gap.
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        // `#{seq}` makes every function/prototype QN unique per file, so C never
        // needs the walker's collision dedup. Matches the hand-written walker.
        format!("{scope}::{name}#{seq}")
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        // The callee text's last segment after a member/scope access: `printf` →
        // `printf`, `obj.method` → `method`, `ptr->call` → `call`. A
        // non-identifier callee (`(fp)()`) is dropped. Reproduces the
        // hand-written `extract_calls` split on `['.', '>', ':']`.
        let callee = node_field_text(source, call_node, C_CALL_FUNCTION_FIELD);
        let tail = callee
            .rsplit(['.', '>', ':'])
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
        _spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        // One `#include` → one import. Strip the directive and the `<>`/`""`
        // delimiters; the display name is the path's last segment. Reproduces
        // the hand-written `extract_include`.
        let text = node_text(source, import_stmt);
        let cleaned = text
            .trim()
            .trim_start_matches("#include")
            .trim()
            .trim_matches('<')
            .trim_matches('>')
            .trim_matches('"')
            .trim()
            .to_string();
        if cleaned.is_empty() {
            return Vec::new();
        }
        let display_name = cleaned.rsplit('/').next().unwrap_or(&cleaned).to_string();
        vec![ImportEntry {
            display_name,
            qualified_name: qual(scope, &format!("include:{cleaned}")),
            ref_to: cleaned.clone(),
            properties: vec![("path".to_string(), cleaned)],
            visibility: "public".to_string(),
            start_line: import_stmt.start_position().row as u64 + 1,
            end_line: import_stmt.end_position().row as u64 + 1,
        }]
    }
}

static C_CONVENTIONS: CConventions = CConventions;

/// The C-family structural sub-table (ADR-0055 phase 6). All node kinds:
/// tree-sitter-c 0.23.4 node-types.json (validated by the spec guard). Shared,
/// unchanged, by the future C++/ObjC migrations.
static C_FAMILY: CFamilySpec = CFamilySpec {
    struct_like_kinds: &["struct_specifier", "union_specifier"],
    enum_like_kinds: &["enum_specifier"],
    enum_member_kinds: &["enumerator"],
    typedef_kinds: &["type_definition"],
    func_def_kinds: &["function_definition"],
    func_decl_kinds: &["declaration"],
    field_decl_kinds: &["field_declaration"],
    func_declarator_kind: "function_declarator",
    init_declarator_kind: "init_declarator",
    declarator_field: "declarator",
    parameters_field: "parameters",
    identifier_kinds: &["identifier", "type_identifier"],
    field_identifier_kind: "field_identifier",
    macro_object_kinds: &["preproc_def"],
    macro_function_kinds: &["preproc_function_def"],
};

/// The C language spec row. Class-model slices are empty; the flat C-family
/// walker consumes `c_family`. All node-kind strings: tree-sitter-c 0.23.4
/// node-types.json (validated by `guard::every_spec_node_kind_is_real_for_its_grammar`).
pub(crate) static C_SPEC: LangSpec = LangSpec {
    language: Language::C,
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
    base_node_kinds: &[],
    type_decl_node_kinds: &[],
    type_spec_node_kinds: &[],
    struct_type_kind: None,
    interface_type_kind: None,
    field_container_kinds: &[],
    field_node_kinds: &[],
    variable_field_kinds: &[],
    body_wrapper_kinds: &[],
    class_body_kinds: &[],
    function_body_kinds: &[],
    value_decl_node_kinds: &[],
    value_spec_node_kinds: &[],
    // Unused by the flat walker (C has no `value_decl` path); a real kind so the
    // guard's unconditional `value_name_kind` check passes.
    value_name_kind: "identifier",
    variable_declarator_kind: None,
    import_node_kinds: &["preproc_include"],
    import_spec_kinds: &[],
    call_node_kinds: &["call_expression"],
    name_field: "name",
    body_field: Some("body"),
    type_field: "type",
    receiver_field: None,
    import_path_field: None,
    extends_field: None,
    value_name_field: None,
    value_type_field: None,
    ts_language: || tree_sitter_c::LANGUAGE.into(),
    embedded: &[],
    conventions: &C_CONVENTIONS,
    c_family: Some(&C_FAMILY),
    cpp_family: None,
};
