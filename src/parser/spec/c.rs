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

use super::c_family;
use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::lang_spec::{CFamilySpec, DeclaratorNaming, LangSpec};
use crate::parser::Language;

/// The tree-sitter-c field naming a call expression's callee. Used only by the
/// conventions (not the generic walkers), so it is a local const rather than a
/// spec field — mirroring `go.rs`'s `GO_CALL_FUNCTION_FIELD`.
/// source: tree-sitter-c 0.23.4 node-types.json (call_expression.function).
const C_CALL_FUNCTION_FIELD: &str = "function";

/// C behavioral conventions. C has no visibility keyword (everything is
/// `public`) and no receiver concept; the only real behavior is `#include`
/// shaping, member-access callee extraction, and the `#{seq}` QN — all now the
/// shared C-family helpers (`c_family`), which C, C++, and Objective-C reuse
/// verbatim (§3.3 rule-of-three).
pub(super) struct CConventions;

impl LanguageConventions for CConventions {
    fn visibility_of(&self, _name: &str) -> String {
        c_family::public_visibility()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // C has no methods / receivers; never reached (no `method_node_kinds`,
        // and the flat `clike` walker has no receiver path).
        // mutation note (§12): mutants of this body SURVIVE and are EQUIVALENT
        // for C — the flat walker never calls `receiver_type`, so no observable
        // output depends on it. It is a required trait obligation, not a live
        // C code path. Not a coverage gap.
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        c_family::def_qn(scope, name, seq)
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        // The callee text's last segment after a member/scope access: `printf` →
        // `printf`, `obj.method` → `method`, `ptr->call` → `call`. A
        // non-identifier callee (`(fp)()`) is dropped. Reproduces the
        // hand-written `extract_calls` split on `['.', '>', ':']`.
        c_family::member_access_callee(source, call_node, C_CALL_FUNCTION_FIELD)
    }

    fn call_entry(
        &self,
        _source: &str,
        call_node: Node,
        caller_qn: &str,
        callee: &str,
        seq: u64,
    ) -> CallEntry {
        c_family::call_entry(call_node, caller_qn, callee, seq)
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
        c_family::include_entry(source, import_stmt, scope, &["#include"], "include:")
    }
}

static C_CONVENTIONS: CConventions = CConventions;

/// How C spells a declaration's name (issue #106). The SAME sub-table type C++
/// carries, consumed by the ONE shared `declarator::declarator_name` — so the
/// "name from the declarator chain, never from `parameters`" rule is data here,
/// not a branch in the walker.
/// source: tree-sitter-c 0.23.4 node-types.json (function_declarator has
/// `declarator` + `parameters`; C has no operator/destructor name kinds, hence
/// the empty `name_text_kinds`).
static C_NAMING: DeclaratorNaming = DeclaratorNaming {
    identifier_kinds: &["identifier", "type_identifier"],
    name_text_kinds: &[],
    declarator_field: "declarator",
    parameters_field: "parameters",
    // C has `pointer_declarator` but no reference declarator.
    indirection_declarator_kinds: &["pointer_declarator"],
};

/// The C-family structural sub-table (ADR-0055 phase 6). All node kinds:
/// tree-sitter-c 0.23.4 node-types.json (validated by the spec guard).
static C_FAMILY: CFamilySpec = CFamilySpec {
    struct_like_kinds: &["struct_specifier", "union_specifier"],
    enum_like_kinds: &["enum_specifier"],
    enum_member_kinds: &["enumerator"],
    typedef_kinds: &["type_definition"],
    func_def_kinds: &["function_definition"],
    func_decl_kinds: &["declaration"],
    field_decl_kinds: &["field_declaration"],
    func_declarator_kind: "function_declarator",
    naming: &C_NAMING,
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
    objc_family: None,
    ts_family: None,
    ts_language_by_ext: None,
};
