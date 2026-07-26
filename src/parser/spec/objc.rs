// parser::spec::objc — the Objective-C LangSpec row + ObjcConventions +
// ObjcFamilySpec (ADR-0055 phase 8).
//
// Objective-C is the third C-family language, and the one that fits no existing
// lane: a C superset carrying an ObjC object model (classes, categories,
// protocols, selector-named methods, message sends). Its extraction routes
// through the dedicated `objc` walker (`walkers/objc.rs`), so this row carries
// an `ObjcFamilySpec` (`objc_family: Some(&OBJC_FAMILY)`) and leaves the
// class-model slices empty. `walk_defs` delegates the whole file to that walker.
//
// The behavioral escape hatch (ADR-0055 §4) holds four predicates:
//   - `public` visibility, `#{seq}` def QN, and the shared C-family `call_entry`
//     — all lifted to `c_family` (the §3.3 rule-of-three extraction).
//   - `#import`/`#include` shaping — the shared `c_family::include_entry` — PLUS
//     the ObjC-only `@import Module;` (`module_import`) shaping.
//   - The call callee: a `message_expression` reconstructs its selector callee
//     (`[obj do:x andThen:y]` → `do:andThen:`); a plain `call_expression` splits
//     its callee on `.` ONLY (`obj.method` → `method`) — deliberately narrower
//     than the C/C++ `['.', '>', ':']` split, preserved for parity.
//
// Every node-kind / field string traces to tree-sitter-objc 3.0.2's
// node-types.json (tree-sitter-grammars/tree-sitter-objc, pinned in Cargo.lock);
// the spec-validation guard (`guard::every_spec_node_kind_is_real_for_its_grammar`)
// asserts it. NOTE: this is the tree-sitter-grammars grammar, NOT
// jiyee/tree-sitter-objc — categories are a `category` FIELD (no
// category_interface node kind), and a `method_declaration`'s selector is a bare
// `identifier` followed by `method_parameter` nodes (NOT `keyword_declarator`),
// which is why a keyword selector resolves to its first keyword only (#128).
// source: tree-sitter-objc 3.0.2 src/node-types.json.

use tree_sitter::Node;

use super::c_family;
use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::lang_spec::{LangSpec, ObjcFamilySpec};
use crate::parser::{node_field_text, node_text, qual, Language};

/// The `call_expression` field naming the callee.
/// source: tree-sitter-objc 3.0.2 node-types.json (call_expression.function).
const OBJC_CALL_FUNCTION_FIELD: &str = "function";
/// The `message_expression` field naming each selector keyword (`multiple`).
/// source: tree-sitter-objc 3.0.2 node-types.json (message_expression.method).
const OBJC_MESSAGE_METHOD_FIELD: &str = "method";
/// The `message_expression` field naming the receiver (skipped when counting
/// arguments). source: tree-sitter-objc 3.0.2 node-types.json.
const OBJC_MESSAGE_RECEIVER_FIELD: &str = "receiver";
/// The message-send node kind, distinguished from `call_expression` so the
/// callee is a reconstructed selector. source: tree-sitter-objc 3.0.2.
const OBJC_MESSAGE_KIND: &str = "message_expression";

/// Objective-C behavioral conventions. Visibility/`#{seq}`/`call_entry` are the
/// shared C-family helpers; `#import` shaping is shared with a `@import` branch
/// added; the call callee reconstructs a message selector or splits a plain call
/// on `.` only.
pub(super) struct ObjcConventions;

impl LanguageConventions for ObjcConventions {
    fn visibility_of(&self, _name: &str) -> String {
        c_family::public_visibility()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // ObjC methods scope by the enclosing class QN, set directly in the
        // `objc` walker (not via a receiver field); this trait method is never
        // called for ObjC.
        // mutation note (§12): mutants of this body SURVIVE and are EQUIVALENT —
        // no ObjC path reads it. A required trait obligation. Not a coverage gap.
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        c_family::def_qn(scope, name, seq)
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        if call_node.kind() == OBJC_MESSAGE_KIND {
            // `[receiver do:x andThen:y]` → `do:andThen:` (keyword send) or
            // `[receiver start]` → `start` (unary send). Reproduces the hand-
            // written `message_selector` + `emit_call`'s alphabetic guard.
            let sel = message_selector(source, call_node);
            if sel.is_empty() {
                return None;
            }
            c_family::identifier_callee(sel)
        } else {
            // `printf(x)` → `printf`, `obj.method(x)` → `method`. Splits on `.`
            // ONLY (not the C/C++ `['.', '>', ':']`); reproduces the hand-written
            // `call_expression` callee.
            let callee = node_field_text(source, call_node, OBJC_CALL_FUNCTION_FIELD);
            let tail = callee
                .rsplit('.')
                .next()
                .unwrap_or("")
                .trim_end_matches('(')
                .to_string();
            c_family::identifier_callee(tail)
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
        c_family::call_entry(call_node, caller_qn, callee, seq)
    }

    fn imports_of(
        &self,
        source: &str,
        _spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        // Two import statement kinds route here (both in `import_node_kinds`):
        // `preproc_include` (`#import`/`#include`) shares the C-family shaping;
        // `module_import` (`@import UIKit;`) has its own shape.
        if import_stmt.kind() == "module_import" {
            module_import_entry(source, import_stmt, scope)
        } else {
            c_family::include_entry(
                source,
                import_stmt,
                scope,
                &["#import", "#include"],
                "import:",
            )
        }
    }
}

/// Reconstructs a message-send selector from a `message_expression`: the
/// `method` field's keywords (one `identifier` per keyword) plus the non-field
/// named argument count decide keyword (`do:andThen:`) vs unary (`start`).
/// Reproduces the hand-written `message_selector`.
fn message_selector(source: &str, node: Node) -> String {
    let mut keywords: Vec<String> = Vec::new();
    let mut arg_count: usize = 0;
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let field = cursor.field_name();
            let child = cursor.node();
            match field {
                Some(OBJC_MESSAGE_METHOD_FIELD) => keywords.push(node_text(source, child)),
                Some(OBJC_MESSAGE_RECEIVER_FIELD) => {}
                _ => {
                    // Arguments are the non-field NAMED children (concrete
                    // `expression` subtypes). Unnamed tokens (`[` `]` `:`) are
                    // skipped by is_named(). A keyword selector always has >=1
                    // argument; a unary selector has none.
                    if field.is_none() && child.is_named() {
                        arg_count += 1;
                    }
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    if keywords.is_empty() {
        return String::new();
    }
    if arg_count > 0 {
        keywords.iter().map(|k| format!("{k}:")).collect::<String>()
    } else {
        keywords.join("")
    }
}

/// One `@import Module;` → one `Import`. Strip the `@import` keyword and the
/// trailing `;`; the display name and edge target are the full remaining text
/// (`UIKit`), NOT a last-path-segment (unlike `#import`). Reproduces the hand-
/// written `extract_module_import`.
fn module_import_entry(source: &str, node: Node, scope: &str) -> Vec<ImportEntry> {
    let cleaned = node_text(source, node)
        .trim()
        .trim_start_matches("@import")
        .trim_end_matches(';')
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return Vec::new();
    }
    vec![ImportEntry {
        display_name: cleaned.clone(),
        qualified_name: qual(scope, &format!("import:{cleaned}")),
        ref_to: cleaned.clone(),
        properties: vec![("path".to_string(), cleaned.clone())],
        visibility: c_family::public_visibility(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
    }]
}

static OBJC_CONVENTIONS: ObjcConventions = ObjcConventions;

/// The Objective-C structural sub-table (ADR-0055 phase 8). All node kinds:
/// tree-sitter-objc 3.0.2 node-types.json (validated by the spec guard).
static OBJC_FAMILY: ObjcFamilySpec = ObjcFamilySpec {
    class_kinds: &["class_interface", "class_implementation"],
    protocol_kinds: &["protocol_declaration"],
    method_kinds: &["method_declaration", "method_definition"],
    func_def_kinds: &["function_definition"],
    struct_kinds: &["struct_specifier", "union_specifier"],
    enum_kinds: &["enum_specifier"],
    enum_member_kinds: &["enumerator"],
    typedef_kinds: &["type_definition"],
    field_decl_kinds: &["field_declaration"],
    category_field: "category",
    superclass_field: "superclass",
    declarator_field: "declarator",
    func_body_kinds: &["compound_statement"],
    field_identifier_kind: "field_identifier",
    // The hand-written `find_name` also listed `class_name`, but tree-sitter-objc
    // 3.0.2 has no such node kind (the spec guard rejects it) — it was a dead
    // branch that never matched (the `name` field always resolves a class/
    // protocol name first). Dropping it is behavior-preserving (§9: no dead
    // reference); the ObjC parity suite proves the output is unchanged.
    identifier_kinds: &["identifier", "type_identifier"],
    plain_identifier_kind: "identifier",
    method_parameter_kinds: &["method_parameter"],
    typedef_name_kind: "type_identifier",
};

/// The Objective-C language spec row. Class-model slices are empty; the ObjC
/// walker consumes `objc_family`. `import_node_kinds` carries BOTH import
/// statement kinds (`preproc_include` and `module_import`); `call_node_kinds`
/// carries BOTH call kinds (`call_expression` and `message_expression`); the
/// conventions branch on the node kind. All node-kind strings: tree-sitter-objc
/// 3.0.2 node-types.json (validated by
/// `guard::every_spec_node_kind_is_real_for_its_grammar`).
pub(crate) static OBJC_SPEC: LangSpec = LangSpec {
    language: Language::ObjC,
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
    // Unused by the ObjC walker (no `value_decl` path); a real kind so the
    // guard's unconditional `value_name_kind` check passes.
    value_name_kind: "identifier",
    variable_declarator_kind: None,
    import_node_kinds: &["preproc_include", "module_import"],
    import_spec_kinds: &[],
    call_node_kinds: &["call_expression", "message_expression"],
    name_field: "name",
    body_field: Some("body"),
    type_field: "type",
    receiver_field: None,
    import_path_field: None,
    extends_field: None,
    value_name_field: None,
    value_type_field: None,
    ts_language: || tree_sitter_objc::LANGUAGE.into(),
    embedded: &[],
    conventions: &OBJC_CONVENTIONS,
    c_family: None,
    cpp_family: None,
    objc_family: Some(&OBJC_FAMILY),
    ts_family: None,
    ts_language_by_ext: None,
    rust_family: None,
};
