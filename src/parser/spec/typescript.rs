// parser::spec::typescript — the TypeScript LangSpec row + TsConventions +
// TsFamilySpec (ADR-0055 phase 9).
//
// TypeScript is class-model-shaped but fits no existing lane: its extraction
// routes through the dedicated `typescript` walker (`walkers/typescript`), so
// this row carries a `TsFamilySpec` (`ts_family: Some(&TS_FAMILY)`) and the
// six structural divergences that motivate the dedicated lane are documented on
// `TsFamilySpec` itself. `walk_defs` delegates the whole file to that walker.
//
// TypeScript is the only language whose grammar depends on the FILE EXTENSION:
// tree-sitter-typescript ships two grammars, `typescript` and `tsx`. JSX
// (`<Component/>`) is ONLY in the tsx grammar — parsing a `.tsx`/`.jsx` file
// with the typescript grammar makes every JSX element an ERROR node and drops
// the symbols inside it. JS-family files (`.js`/`.jsx`/`.mjs`/`.cjs`) route here
// too and carry no type syntax, so tsx is safe for them. This is expressed via
// `ts_language_by_ext` (a grammar factory keyed on the path); `ts_language` is
// the typescript grammar as the fixed fallback (and the grammar the spec guard
// validates against — every node kind below is a core-TS kind present in both
// grammars). source: tree-sitter-typescript 0.23.2 (typescript vs tsx
// node-types.json); cross-ref GitNexus parser-loader.ts selects `:tsx` for .tsx.
//
// The behavioral escape hatch (ADR-0055 §4) holds four USED predicates —
// `def_qn` (`{scope}::{name}`, no seq, so a getter/setter pair collides on one
// QN by construction, no dedup), `call_callee`/`call_entry` (the byte-span
// call-site key + the `Calls`-to-tail edge), and `imports_of`/`import_ref_kind`
// (the ES-module import-clause shapes, edged `Defines`). Two required methods
// (`visibility_of`, `receiver_type`) are inapplicable to TypeScript — its
// visibility is structural (`export`/`accessibility_modifier`, computed in the
// walker) and it has no receiver-field concept — so they are trivial and never
// called; see the §12 notes.
//
// Every node-kind / field string traces to tree-sitter-typescript 0.23.2's
// node-types.json (pinned in Cargo.lock); the spec-validation guard
// (`guard::every_spec_node_kind_is_real_for_its_grammar`) asserts it.

use tree_sitter::{Language as TsLanguage, Node};

use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::lang_spec::{LangSpec, TsFamilySpec};
use crate::parser::{node_text, qual, Language};

/// The `call_expression` field naming the callee.
/// source: tree-sitter-typescript 0.23.2 node-types.json (call_expression.function).
const TS_CALL_FUNCTION_FIELD: &str = "function";

/// Selects the tree-sitter grammar for a TypeScript-family file by extension:
/// `tsx` for `.tsx`/`.jsx`/`.js`/`.mjs`/`.cjs` (JSX-capable, JS-safe),
/// `typescript` otherwise. Reproduces the hand-written `parse_typescript_file`
/// grammar choice. source: tree-sitter-typescript 0.23.2.
fn ts_grammar_for(file_path: &str) -> TsLanguage {
    let use_tsx = file_path
        .rsplit('.')
        .next()
        .map(|ext| matches!(ext, "tsx" | "jsx" | "js" | "mjs" | "cjs"))
        .unwrap_or(false);
    if use_tsx {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }
}

/// TypeScript behavioral conventions. Only `def_qn`, `call_callee`,
/// `call_entry`, `imports_of`, and `import_ref_kind` are exercised; the
/// remaining required methods are inapplicable (see the module header).
pub(super) struct TsConventions;

impl LanguageConventions for TsConventions {
    fn visibility_of(&self, _name: &str) -> String {
        // TypeScript visibility is structural (an `export` wrapper → `pub`; an
        // `accessibility_modifier` child → its text), computed in the walker;
        // it is never name-based, so this method is never called for TS.
        // mutation note (§12): mutants of this body SURVIVE and are EQUIVALENT —
        // no TS path reads it. A required trait obligation. Not a coverage gap.
        String::new()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // TS methods scope by the enclosing class/interface QN, set directly in
        // the `typescript` walker (not via a receiver field); never called.
        // mutation note (§12): mutants SURVIVE and are EQUIVALENT (no TS reader).
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, _seq: u64) -> String {
        // `{scope}::{name}` — no `#seq` disambiguation. A getter and setter of
        // the same name therefore collide on ONE QN, which the walker emits
        // WITHOUT dedup (two `Method` nodes share the QN), reproducing the
        // hand-written walker exactly. `seq` is unused.
        qual(scope, name)
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        // The `function` field's text is the callee (`greet`, `Promise.resolve`,
        // `this.fetch`, `super`). An empty callee drops the call. Reproduces the
        // hand-written `extract_single_call_site`.
        let func = call_node.child_by_field_name(TS_CALL_FUNCTION_FIELD)?;
        let callee = node_text(source, func);
        if callee.is_empty() {
            None
        } else {
            Some(callee)
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
        // The call-site QN is keyed by byte span (`call@line:col#start-end`),
        // uniquely identifying even chained calls `f()()` that share a start
        // byte. The `Calls` edge targets the LAST dotted segment
        // (`Promise.resolve` → `resolve`), matching the sister extractors'
        // `callee_tail` convention; the CallSite node keeps the FULL callee text
        // as both its name and its `callee_name` property. `seq` is unused (TS
        // keys on the byte span, not the per-file counter).
        let line = call_node.start_position().row as u64 + 1;
        let col = call_node.start_position().column as u64;
        let start_byte = call_node.start_byte() as u64;
        let end_byte = call_node.end_byte() as u64;
        let cs_id = format!("{caller_qn}::call@{line}:{col}#{start_byte}-{end_byte}");
        let callee_tail = callee.rsplit('.').next().unwrap_or(callee).to_string();
        CallEntry {
            name: callee.to_string(),
            qualified_name: cs_id,
            visibility: String::new(),
            properties: vec![
                ("callee_name".to_string(), callee.to_string()),
                ("caller_qn".to_string(), caller_qn.to_string()),
                // source: LSP 3.17 Base Protocol — positions are 0-based;
                // `col` above is already 0-based here, so `lsp_col` mirrors
                // it verbatim. Read by
                // indexer::persist::nodes::append_label_properties.
                ("lsp_col".to_string(), col.to_string()),
            ],
            start_line: line,
            end_line: line,
            ref_kind: "Calls",
            ref_to: callee_tail,
        }
    }

    fn import_ref_kind(&self, _import_stmt: Node) -> &'static str {
        // TS imports are file-local `Defines` edges (like Python), not `Imports`.
        // The statement node is unused: TS has a single import edge kind, unlike
        // Rust which reads it to split `use` (Defines) from `extern crate` (Imports).
        "Defines"
    }

    fn imports_of(
        &self,
        source: &str,
        spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        let tf = match spec.ts_family {
            Some(tf) => tf,
            // Unreachable: `imports_of` is only called for the TS row, which
            // always carries `ts_family`. A defensive empty keeps the fn total.
            None => return Vec::new(),
        };
        ts_imports_of(tf, source, import_stmt, scope)
    }
}

/// Interprets one `import_statement` into zero or more `ImportEntry`.
/// Reproduces the hand-written `extract_import`: read the `source` module
/// string (quotes stripped, `/` → `::`), then dispatch each `import_clause`
/// child; a statement with no clause is a side-effect import
/// (`import './x'`). Every entry is edged `Defines(scope → its own QN)`.
fn ts_imports_of(
    tf: &TsFamilySpec,
    source: &str,
    import_stmt: Node,
    scope: &str,
) -> Vec<ImportEntry> {
    let start_line = import_stmt.start_position().row as u64 + 1;
    let end_line = import_stmt.end_position().row as u64 + 1;
    let module_path = import_stmt
        .child_by_field_name(tf.source_field)
        .map(|n| {
            node_text(source, n)
                .trim_matches(|c| c == '\'' || c == '"')
                .replace('/', "::")
        })
        .unwrap_or_default();

    let mut out: Vec<ImportEntry> = Vec::new();
    let mut found_clause = false;
    let mut cursor = import_stmt.walk();
    for child in import_stmt.children(&mut cursor) {
        if kind_in(tf.import_clause_kinds, child.kind()) {
            found_clause = true;
            collect_import_clause(
                tf,
                source,
                child,
                scope,
                &module_path,
                start_line,
                end_line,
                &mut out,
            );
        }
    }
    if !found_clause && !module_path.is_empty() {
        // Side-effect import: `import 'foo'`.
        push_import(
            scope,
            &module_path,
            "",
            false,
            start_line,
            end_line,
            &mut out,
        );
    }
    out
}

/// Dispatches an `import_clause`'s children: a bare `identifier` is a default
/// import (`{path}::default`); a `named_imports` group yields one entry per
/// specifier; a `namespace_import` is a glob (`import * as x`).
#[allow(clippy::too_many_arguments)]
fn collect_import_clause(
    tf: &TsFamilySpec,
    source: &str,
    clause: Node,
    scope: &str,
    module_path: &str,
    start_line: u64,
    end_line: u64,
    out: &mut Vec<ImportEntry>,
) {
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        let k = child.kind();
        if kind_in(tf.default_import_ident_kinds, k) {
            let name = node_text(source, child);
            let full_path = format!("{module_path}::default");
            push_import(scope, &full_path, &name, false, start_line, end_line, out);
        } else if kind_in(tf.named_imports_kinds, k) {
            collect_named_imports(
                tf,
                source,
                child,
                scope,
                module_path,
                start_line,
                end_line,
                out,
            );
        } else if kind_in(tf.namespace_import_kinds, k) {
            let alias = namespace_alias(tf, source, child);
            push_import(scope, module_path, &alias, true, start_line, end_line, out);
        }
    }
}

/// One entry per `import_specifier` in a `named_imports` group. The path is
/// `{module}::{name}`; the display name is the alias when present, else the path.
#[allow(clippy::too_many_arguments)]
fn collect_named_imports(
    tf: &TsFamilySpec,
    source: &str,
    group: Node,
    scope: &str,
    module_path: &str,
    start_line: u64,
    end_line: u64,
    out: &mut Vec<ImportEntry>,
) {
    let mut cursor = group.walk();
    for child in group.children(&mut cursor) {
        if !kind_in(tf.import_specifier_kinds, child.kind()) {
            continue;
        }
        let name = child
            .child_by_field_name("name")
            .map(|n| node_text(source, n))
            .unwrap_or_default();
        let alias = child
            .child_by_field_name(tf.alias_field)
            .map(|n| node_text(source, n))
            .unwrap_or_default();
        let full_path = format!("{module_path}::{name}");
        push_import(scope, &full_path, &alias, false, start_line, end_line, out);
    }
}

/// The alias of a `namespace_import` (`import * as utils`): its `name` field,
/// falling back to its first `identifier` child.
fn namespace_alias(tf: &TsFamilySpec, source: &str, node: Node) -> String {
    if let Some(n) = node.child_by_field_name("name") {
        return node_text(source, n);
    }
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| kind_in(tf.default_import_ident_kinds, c.kind()))
        .map(|c| node_text(source, c))
        .unwrap_or_default();
    found
}

/// Builds one `ImportEntry`. Display name: the alias when non-empty, else
/// `{path}::*` for a glob, else the path. The entry's QN is its own primary key
/// and the `Defines` edge target. Reproduces the hand-written `emit_ts_import`.
fn push_import(
    scope: &str,
    path: &str,
    alias: &str,
    is_glob: bool,
    start_line: u64,
    end_line: u64,
    out: &mut Vec<ImportEntry>,
) {
    if path.is_empty() {
        return;
    }
    let display_name = if !alias.is_empty() {
        alias.to_string()
    } else if is_glob {
        format!("{path}::*")
    } else {
        path.to_string()
    };
    let qn = qual(scope, &display_name);
    out.push(ImportEntry {
        display_name,
        qualified_name: qn.clone(),
        ref_to: qn,
        properties: vec![
            ("path".to_string(), path.to_string()),
            ("alias".to_string(), alias.to_string()),
            ("is_glob".to_string(), is_glob.to_string()),
        ],
        visibility: String::new(),
        start_line,
        end_line,
    });
}

/// Local copy of the walkers' `kind_in` (this module is a sibling of `walkers`,
/// not a child, so it cannot reach the `pub(super)` helper).
fn kind_in(kinds: &[&str], k: &str) -> bool {
    kinds.contains(&k)
}

static TS_CONVENTIONS: TsConventions = TsConventions;

/// The TypeScript structural sub-table (ADR-0055 phase 9). All node kinds and
/// field names: tree-sitter-typescript 0.23.2 node-types.json (validated by the
/// spec guard).
static TS_FAMILY: TsFamilySpec = TsFamilySpec {
    type_alias_kinds: &["type_alias_declaration"],
    export_kinds: &["export_statement"],
    method_def_kinds: &["method_definition"],
    field_def_kinds: &["public_field_definition"],
    method_sig_kinds: &["method_signature"],
    property_sig_kinds: &["property_signature"],
    class_body_kinds: &["class_body"],
    interface_body_kinds: &["interface_body", "object_type"],
    enum_body_kinds: &["enum_body"],
    enum_assignment_kinds: &["enum_assignment"],
    enum_member_ident_kinds: &["property_identifier"],
    heritage_kinds: &["class_heritage"],
    extends_clause_kinds: &["extends_clause"],
    implements_clause_kinds: &["implements_clause"],
    interface_extends_kinds: &["extends_type_clause"],
    heritage_name_kinds: &["identifier", "type_identifier"],
    generic_type_kinds: &["generic_type"],
    import_clause_kinds: &["import_clause"],
    named_imports_kinds: &["named_imports"],
    namespace_import_kinds: &["namespace_import"],
    import_specifier_kinds: &["import_specifier"],
    default_import_ident_kinds: &["identifier"],
    declarator_kinds: &["variable_declarator"],
    arrow_kinds: &["arrow_function"],
    accessibility_kinds: &["accessibility_modifier"],
    abstract_method_sig_kinds: &["abstract_method_signature"],
    object_literal_kinds: &["object"],
    pair_kinds: &["pair"],
    source_field: "source",
    value_field: "value",
    alias_field: "alias",
};

/// The TypeScript language spec row. Structure lives in `ts_family`; the main
/// slices carry only the kinds the dedicated walker reads directly
/// (function/class/interface/enum/value-decl/import/call) so they are validated
/// by the standard guard slices. All node-kind strings: tree-sitter-typescript
/// 0.23.2 node-types.json (validated by
/// `guard::every_spec_node_kind_is_real_for_its_grammar`).
pub(crate) static TS_SPEC: LangSpec = LangSpec {
    language: Language::TypeScript,
    skip_node_kinds: &[],
    function_node_kinds: &["function_declaration", "generator_function_declaration"],
    method_node_kinds: &[],
    class_node_kinds: &["class_declaration", "abstract_class_declaration"],
    interface_node_kinds: &["interface_declaration"],
    enum_node_kinds: &["enum_declaration"],
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
    value_decl_node_kinds: &["lexical_declaration", "variable_declaration"],
    value_spec_node_kinds: &[],
    // Unused by the TS walker (it reads `variable_declarator` via `ts_family`);
    // a real kind so the guard's unconditional `value_name_kind` check passes.
    value_name_kind: "identifier",
    variable_declarator_kind: None,
    import_node_kinds: &["import_statement"],
    import_spec_kinds: &[],
    call_node_kinds: &["call_expression"],
    // `new OrderConfig()` (issue #92); `constructor` names the constructed type.
    type_construction_kinds: &["new_expression"],
    name_field: "name",
    body_field: Some("body"),
    type_field: "type",
    receiver_field: None,
    import_path_field: None,
    extends_field: None,
    value_name_field: None,
    value_type_field: None,
    // `function f(): OrderConfig` — `return_type` is the `type_annotation` (issue #92).
    return_type_field: Some("return_type"),
    construction_type_field: Some("constructor"),
    ts_language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    embedded: &[],
    conventions: &TS_CONVENTIONS,
    c_family: None,
    cpp_family: None,
    objc_family: None,
    ts_family: Some(&TS_FAMILY),
    ts_language_by_ext: Some(ts_grammar_for),
    rust_family: None,
};
