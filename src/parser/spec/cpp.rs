// parser::spec::cpp — the C++ LangSpec row + CppConventions (ADR-0055 phase 7).
//
// C++ is the first HYBRID C-family class-model language: its extraction routes
// through the dedicated `cpp` walker (see `walkers/cpp.rs`) rather than the flat
// `clike` walker or the class-model arms, so this row carries a `CppFamilySpec`
// (`cpp_family: Some(&CPP_FAMILY)`) and leaves the class-model slices empty. The
// walker is a class-recursive DFS reproducing the hand-written
// `parser::cpp::extract` at exact parity: namespaces/classes/structs recurse
// their bodies, members become `Constant`s or prototype `Method`s, enums emit
// no members, and one per-file `seq` keys functions, prototypes, and call sites.
//
// The behavioral escape hatch (ADR-0055 §4) holds the C-specific predicates C++
// shares with C — `#include` interpretation, the member-access-aware call callee
// (`a->b` / `a.b` / `a::b` → `b`), and the `#{seq}` def QN — PLUS the one thing C
// lacks: `using` directives/declarations (`using namespace std;`,
// `using std::vector;`) as `Import`s. Visibility is uniformly `public` (the
// hand-written walker hardcoded `public` on every node, even private members).
//
// Every node-kind string traces to tree-sitter-cpp 0.23.4's node-types.json
// (github.com/tree-sitter/tree-sitter-cpp, pinned in Cargo.lock); the spec-
// validation guard (`guard::every_spec_node_kind_is_real_for_its_grammar`)
// asserts it.
// source: tree-sitter-cpp 0.23.4 src/node-types.json.

use tree_sitter::Node;

use super::c_family;
use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::lang_spec::{CppFamilySpec, DeclaratorNaming, LangSpec};
use crate::parser::{node_text, qual, Language};

/// The tree-sitter-cpp field naming a call expression's callee. Used only by the
/// conventions (not the generic walkers), so it is a local const rather than a
/// spec field — mirroring `c.rs`'s `C_CALL_FUNCTION_FIELD`.
/// source: tree-sitter-cpp 0.23.4 node-types.json (call_expression.function).
const CPP_CALL_FUNCTION_FIELD: &str = "function";

/// C++ behavioral conventions. C++ has visibility keywords, but the hand-written
/// walker ignored them and hardcoded `public` on every node (including private
/// members) — parity preserves that. The genuine behavior is `#include`/`using`
/// import shaping, member-access callee extraction, and the `#{seq}` QN.
///
/// The member-access callee (`call_callee`/`call_entry`), the `#{seq}` QN, the
/// `public` visibility, and the `#include` shaping are now the shared C-family
/// helpers (`c_family`) — the phase-8 (ObjC) rule-of-three extraction (§3.3),
/// behavior-preserving for C++ (this file's parity suite proves it). The `using`
/// branch below is genuinely C++-specific and stays here.
pub(super) struct CppConventions;

impl LanguageConventions for CppConventions {
    fn visibility_of(&self, _name: &str) -> String {
        // The hand-written walker emitted `public` on every node regardless of
        // the C++ access specifier (`private:`/`protected:`); parity preserves it.
        c_family::public_visibility()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // C++ methods scope by enclosing class (threaded as `enclosing_type` in
        // the `cpp` walker), not by a receiver field; this trait method is never
        // called for C++ (no `method_node_kinds`).
        // mutation note (§12): mutants of this body SURVIVE and are EQUIVALENT —
        // the `cpp` walker never calls `receiver_type`. A required trait
        // obligation, not a live C++ path. Not a coverage gap.
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        c_family::def_qn(scope, name, seq)
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        // The callee text's last segment after a member/scope access: `printf` →
        // `printf`, `obj.method` → `method`, `ptr->call` → `call`,
        // `geometry::identity` → `identity`. A non-identifier callee (`(fp)()`)
        // is dropped. Reproduces the hand-written `extract_calls` split on
        // `['.', '>', ':']`.
        c_family::member_access_callee(source, call_node, CPP_CALL_FUNCTION_FIELD)
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
        // Two import statement kinds route here (both listed in
        // `import_node_kinds`): `preproc_include` and `using_declaration`. They
        // shape differently, matching the hand-written `extract_include` /
        // `extract_using`.
        match import_stmt.kind() {
            "using_declaration" => using_entry(source, import_stmt, scope),
            // `preproc_include` and any other kind fall to the include shaping —
            // the walker only routes these two kinds here, so the arm is the
            // shared C-family include path (matching the hand-written
            // `extract_include`).
            _ => c_family::include_entry(source, import_stmt, scope, &["#include"], "include:"),
        }
    }
}

/// One `using` directive/declaration → one `Import` (`using=true`). Strip the
/// `using` keyword and the trailing `;`; the display name and edge target are
/// the remaining text (`namespace std`, `std::vector`). Reproduces the
/// hand-written `extract_using`.
fn using_entry(source: &str, node: Node, scope: &str) -> Vec<ImportEntry> {
    let text = node_text(source, node);
    let cleaned = text
        .trim()
        .trim_start_matches("using")
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return Vec::new();
    }
    vec![ImportEntry {
        display_name: cleaned.clone(),
        qualified_name: qual(scope, &format!("using:{cleaned}")),
        ref_to: cleaned.clone(),
        properties: vec![("using".to_string(), "true".to_string())],
        visibility: "public".to_string(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
    }]
}

static CPP_CONVENTIONS: CppConventions = CppConventions;

/// How C++ spells a declaration's name (issue #123). The SAME sub-table type C
/// carries, consumed by the ONE shared `declarator::declarator_name` — this row is
/// how C++ inherits the #106 fix ("the name is what the declarator binds, never a
/// parameter") as DATA rather than as a second implementation.
///
/// `field_identifier` joins the identifier kinds because a class member's
/// declarator binds one (`int magnitude() const;` → `magnitude`).
/// `destructor_name` and `operator_name` are name-BY-TEXT kinds: each wraps an
/// inner `identifier` whose text is the wrong name (`Point` for `~Point`, and
/// nothing at all for `operator+`, whose `+` is an anonymous token), so the search
/// must stop at them.
/// source: tree-sitter-cpp 0.23.4 node-types.json (function_declarator has
/// `declarator` + `parameters`; destructor_name's only child is an `identifier`;
/// operator_name's child list is an optional `identifier`).
static CPP_NAMING: DeclaratorNaming = DeclaratorNaming {
    identifier_kinds: &["identifier", "type_identifier", "field_identifier"],
    name_text_kinds: &["destructor_name", "operator_name"],
    declarator_field: "declarator",
    parameters_field: "parameters",
    // C++ has both pointer and reference declarators; a `(*cb)` or `(&cb)` below
    // an outer `function_declarator` makes the member a function-pointer (#135).
    indirection_declarator_kinds: &["pointer_declarator", "reference_declarator"],
};

/// The hybrid C-family class-model structural sub-table (ADR-0055 phase 7). All
/// node kinds: tree-sitter-cpp 0.23.4 node-types.json (validated by the spec
/// guard). Shared, unchanged, by the future ObjC migration where the shapes
/// overlap.
static CPP_FAMILY: CppFamilySpec = CppFamilySpec {
    namespace_kinds: &["namespace_definition"],
    class_kinds: &["class_specifier"],
    struct_kinds: &["struct_specifier", "union_specifier"],
    enum_kinds: &["enum_specifier"],
    enum_member_kinds: &["enumerator"],
    template_kinds: &["template_declaration"],
    func_def_kinds: &["function_definition"],
    field_decl_kinds: &["field_declaration"],
    member_decl_kinds: &["declaration"],
    typedef_kinds: &["type_definition"],
    alias_kinds: &["alias_declaration"],
    func_declarator_kind: "function_declarator",
    qualified_declarator_kind: "qualified_identifier",
    qualified_scope_field: "scope",
    base_clause_kind: "base_class_clause",
    base_type_kinds: &["type_identifier", "qualified_identifier", "template_type"],
    naming: &CPP_NAMING,
};

/// The C++ language spec row. Class-model slices are empty; the hybrid `cpp`
/// walker consumes `cpp_family`. `import_node_kinds` carries BOTH C++ import
/// statement kinds (`preproc_include` and `using_declaration`); the conventions'
/// `imports_of` branches on the node kind. All node-kind strings:
/// tree-sitter-cpp 0.23.4 node-types.json (validated by
/// `guard::every_spec_node_kind_is_real_for_its_grammar`).
pub(crate) static CPP_SPEC: LangSpec = LangSpec {
    language: Language::Cpp,
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
    // Unused by the hybrid walker (C++ has no `value_decl` path); a real kind so
    // the guard's unconditional `value_name_kind` check passes.
    value_name_kind: "identifier",
    variable_declarator_kind: None,
    import_node_kinds: &["preproc_include", "using_declaration"],
    import_spec_kinds: &[],
    call_node_kinds: &["call_expression"],
    // Not adopted (issue #92): no return-type/type-construction Uses extraction.
    type_construction_kinds: &[],
    name_field: "name",
    body_field: Some("body"),
    type_field: "type",
    receiver_field: None,
    import_path_field: None,
    extends_field: None,
    value_name_field: None,
    value_type_field: None,
    return_type_field: None,
    construction_type_field: None,
    ts_language: || tree_sitter_cpp::LANGUAGE.into(),
    embedded: &[],
    conventions: &CPP_CONVENTIONS,
    c_family: None,
    cpp_family: Some(&CPP_FAMILY),
    objc_family: None,
    ts_family: None,
    ts_language_by_ext: None,
    rust_family: None,
};
