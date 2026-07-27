// parser::spec::typescript — the TypeScript LangSpec row + TypeScriptConventions
// (ADR-0055 phase 7, TypeScript).
//
// TypeScript is the first ECMAScript-family language: its extraction routes
// through the dedicated `ts` walker (see `walkers/ts.rs`) rather than the
// class-model arms or either C-family walker, so this row carries a
// `TsFamilySpec` (`ts_family: Some(&TS_FAMILY)`) and leaves the class-model
// slices empty. The reasons the class-model walker cannot express TypeScript —
// export-as-wrapper visibility, `const f = () => {}` as a `Function`, four
// member kinds across class and interface bodies, and non-deduplicated def QNs
// (a getter/setter pair shares one) — are argued in `TsFamilySpec`'s type doc.
//
// This row is also the first to carry a `dialect`: tree-sitter-typescript ships
// two grammars, and JSX parses only under `tsx`. `.tsx`/`.jsx`/`.js`/`.mjs`/
// `.cjs` select it; `.ts` (and anything else routed to `Language::TypeScript`)
// uses the `typescript` grammar. That is the pre-migration behavior, preserved
// exactly.
//
// The behavioral escape hatch (ADR-0055 §4) holds what will not reduce to node
// kinds: the four ES import shapes (default / named / namespace / side-effect)
// with their `/`→`::` path normalization, the two-edge call site
// (`Defines` to the call-site node + `Calls` to the callee tail), the
// keyword-prefix `async`/`const` predicates, and the non-deduplicating def QN.
//
// Every node-kind string traces to tree-sitter-typescript 0.23.2's
// node-types.json — and is validated against BOTH the `typescript` and `tsx`
// dialects by `guard::every_spec_node_kind_is_real_for_its_grammar`.
// source: tree-sitter-typescript 0.23.2 typescript/src/node-types.json and
// tsx/src/node-types.json (pinned in Cargo.lock).

use tree_sitter::Node;

use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::families::TsFamilySpec;
use super::lang_spec::{DialectSpec, LangSpec};
use crate::parser::{node_field_text, node_text, qual, Language};

/// The tree-sitter-typescript field naming a call expression's callee. Read only
/// by the conventions (not the generic walkers), so it is a local const rather
/// than a spec field — mirroring `c.rs`'s `C_CALL_FUNCTION_FIELD`.
/// source: tree-sitter-typescript 0.23.2 node-types.json (call_expression.function).
const TS_CALL_FUNCTION_FIELD: &str = "function";

/// The `async` keyword prefix that marks a function/method/arrow node async.
/// The hand-written walker sniffed the node's leading TEXT rather than reading
/// the grammar's modifier child, so `async` must be followed by exactly one
/// space to count. Preserved verbatim for parity.
const TS_ASYNC_PREFIX: &str = "async ";

/// The `const` keyword prefix that marks a value-declaration statement's
/// bindings constant. Text-sniffed rather than read from `lexical_declaration`'s
/// `kind` field, matching the hand-written walker (the two differ on exotic
/// whitespace, e.g. `const\tx = 1`, so the text check is the parity-preserving
/// one).
const TS_CONST_PREFIX: &str = "const ";

/// The path segment appended for a default import: `import Foo from 'bar'`
/// records `bar::default`, distinguishing the module's default export from a
/// named one. Matches the hand-written `extract_import_clause`.
const TS_DEFAULT_IMPORT_SEGMENT: &str = "default";

/// TypeScript behavioral conventions.
///
/// Visibility is structural, not name-derived: `export` (or an enclosing
/// `export` wrapper) makes a declaration `pub`, and a class member's
/// `accessibility_modifier` names its own visibility — both handled by the `ts`
/// walker, which owns the wrapper flag and the modifier child. `visibility_of`
/// is therefore the "nothing else said" answer: the empty visibility the
/// hand-written walker emitted for every non-exported declaration.
pub(super) struct TypeScriptConventions;

impl LanguageConventions for TypeScriptConventions {
    fn visibility_of(&self, _name: &str) -> String {
        // A non-exported TypeScript declaration has NO visibility string (not
        // "private"): the hand-written walker emitted `String::new()` and the
        // resolver keys off `pub` only.
        String::new()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // TypeScript methods scope by enclosing class (threaded as the class QN
        // through the `ts` walker), not by a receiver field; this trait method is
        // never called for TypeScript (no `method_node_kinds`).
        // mutation note (§12): mutants of this body SURVIVE and are EQUIVALENT —
        // the `ts` walker never calls `receiver_type`. A required trait
        // obligation, not a live TypeScript path. Not a coverage gap.
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, _seq: u64) -> String {
        // `{scope}::{name}` with NO uniquifying suffix and no walker dedup: a
        // getter/setter pair (`get label()` / `set label()`) deliberately shares
        // one QN, as it did before the migration. `seq` is unused — TypeScript
        // call sites key on their byte span instead.
        qual(scope, name)
    }

    fn function_props(&self, source: &str, node: Node) -> Vec<(String, String)> {
        // `is_async` is ALWAYS present (`"true"`/`"false"`), never omitted.
        vec![(
            "is_async".to_string(),
            has_keyword_prefix(source, node, TS_ASYNC_PREFIX).to_string(),
        )]
    }

    fn is_const_decl(&self, source: &str, node: Node) -> bool {
        has_keyword_prefix(source, node, TS_CONST_PREFIX)
    }

    fn import_ref_kind(&self) -> &'static str {
        // A TypeScript import is a file-local declaration edge (the `Import`
        // node's own QN), not an `Imports` edge to a module path.
        "Defines"
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        // The callee is the `function` field's FULL text — `obj.method` stays
        // `obj.method` (the resolver decides what resolves), and a chained
        // `factory()()` yields the outer callee text `factory()`. Only an empty
        // callee is dropped.
        let callee = node_field_text(source, call_node, TS_CALL_FUNCTION_FIELD);
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
        let line = call_node.start_position().row as u64 + 1;
        let col = call_node.start_position().column as u64;
        // Chained calls (`f()()`) share a start byte, so the (start, end) byte
        // span is what uniquely identifies a call site.
        let start_byte = call_node.start_byte() as u64;
        let end_byte = call_node.end_byte() as u64;
        let qn = format!("{caller_qn}::call@{line}:{col}#{start_byte}-{end_byte}");
        // The last dotted segment (`obj.method` → `method`), matching the
        // `callee_tail` convention the sister-language extractors use for their
        // `Calls` refs.
        let callee_tail = callee.rsplit('.').next().unwrap_or(callee).to_string();
        CallEntry {
            name: callee.to_string(),
            qualified_name: qn.clone(),
            visibility: String::new(),
            properties: vec![
                ("callee_name".to_string(), callee.to_string()),
                ("caller_qn".to_string(), caller_qn.to_string()),
            ],
            start_line: line,
            // A call site is recorded as a single-line node even when the call
            // spans several lines (the hand-written walker's convention).
            end_line: line,
            ref_kind: "Defines",
            ref_to: qn,
            extra_refs: vec![("Calls", callee_tail)],
        }
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
            // Unreachable: only the TypeScript row routes here, and it always
            // carries `ts_family`. Returning no imports (rather than panicking)
            // keeps a misconfigured row a silent no-op the guard would catch.
            None => return Vec::new(),
        };
        let ctx = ImportCtx {
            source,
            tf,
            name_field: spec.name_field,
            scope,
            module: module_path(source, tf, import_stmt),
            import_stmt,
        };
        let mut out: Vec<ImportEntry> = Vec::new();
        let mut cursor = import_stmt.walk();
        let mut saw_clause = false;
        for child in import_stmt.children(&mut cursor) {
            if !kind_in(tf.import_clause_kinds, child.kind()) {
                continue;
            }
            saw_clause = true;
            clause_entries(&ctx, child, &mut out);
        }
        if !saw_clause && !ctx.module.is_empty() {
            // Side-effect import (`import 'reflect-metadata'`): the module path
            // itself is the display name.
            ctx.push(&mut out, &ctx.module, "", false);
        }
        out
    }
}

/// Everything the import shaping needs that is the same for every entry one
/// import STATEMENT yields: the source text, the sub-table, the spec's name
/// field, the enclosing scope, the resolved module path, and the statement node
/// (which supplies every entry's line span).
///
/// A parameter object rather than eight positional arguments (§4.4): the helpers
/// below thread all of it unchanged and only vary in which clause node they
/// read.
struct ImportCtx<'a> {
    source: &'a str,
    tf: &'a TsFamilySpec,
    name_field: &'a str,
    scope: &'a str,
    module: String,
    import_stmt: Node<'a>,
}

impl ImportCtx<'_> {
    /// Appends one `Import` entry for `path`, with this statement's scope and
    /// line span. See `push_import` for the display-name rule.
    fn push(&self, out: &mut Vec<ImportEntry>, path: &str, alias: &str, is_glob: bool) {
        push_import(out, self.scope, path, alias, is_glob, self.import_stmt);
    }
}

/// Whether `kinds` contains `k`. A local copy of the walkers' helper so the
/// conventions do not depend on the walker module.
fn kind_in(kinds: &[&str], k: &str) -> bool {
    kinds.contains(&k)
}

/// Whether the node's source text starts with `prefix` — the hand-written
/// walker's keyword test for `async` and `const`.
fn has_keyword_prefix(source: &str, node: Node, prefix: &str) -> bool {
    source[node.byte_range()].starts_with(prefix)
}

/// The imported module path: the statement's `source` string with its quotes
/// stripped and `/` normalized to `::` (so `'./module'` → `.::module`), matching
/// the QN separator every other label uses. Empty when the statement has no
/// source (`import x = require('y')`).
fn module_path(source: &str, tf: &TsFamilySpec, import_stmt: Node) -> String {
    let raw = node_field_text(source, import_stmt, tf.import_source_field);
    raw.trim_matches(|c| c == '\'' || c == '"')
        .replace('/', "::")
}

/// Interprets one import clause into its entries: a bare identifier is the
/// default import, a namespace import is a glob alias, and a named-import
/// container contributes one entry per specifier.
fn clause_entries(ctx: &ImportCtx, clause: Node, out: &mut Vec<ImportEntry>) {
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        let k = child.kind();
        if kind_in(ctx.tf.default_import_kinds, k) {
            // `import Foo from 'bar'` → path `bar::default`, aliased `Foo`.
            let alias = node_text(ctx.source, child);
            let path = format!("{}::{TS_DEFAULT_IMPORT_SEGMENT}", ctx.module);
            ctx.push(out, &path, &alias, false);
        } else if kind_in(ctx.tf.named_imports_kinds, k) {
            named_import_entries(ctx, child, out);
        } else if kind_in(ctx.tf.namespace_import_kinds, k) {
            // `import * as utils from 'bar'` → the module path itself, glob,
            // aliased by the clause's identifier child. tree-sitter-typescript
            // 0.23.2 gives `namespace_import` NO fields, so the identifier child
            // is the only place the alias lives.
            let mut inner = child.walk();
            let alias = child
                .children(&mut inner)
                .find(|n| kind_in(ctx.tf.default_import_kinds, n.kind()))
                .map(|n| node_text(ctx.source, n))
                .unwrap_or_default();
            let module = ctx.module.clone();
            ctx.push(out, &module, &alias, true);
        }
    }
}

/// One entry per `import_specifier`: path `{module}::{name}`, alias from the
/// specifier's `alias` field (empty when the name is imported unaliased).
fn named_import_entries(ctx: &ImportCtx, container: Node, out: &mut Vec<ImportEntry>) {
    let mut cursor = container.walk();
    for child in container.children(&mut cursor) {
        if !kind_in(ctx.tf.import_specifier_kinds, child.kind()) {
            continue;
        }
        let name = node_field_text(ctx.source, child, ctx.name_field);
        let alias = node_field_text(ctx.source, child, ctx.tf.alias_field);
        let path = format!("{}::{name}", ctx.module);
        ctx.push(out, &path, &alias, false);
    }
}

/// Builds one `Import` entry. The display name is the alias when present, else
/// `{path}::*` for a glob, else the path. An empty path emits nothing. The line
/// span is the whole import STATEMENT's (several entries from one statement share
/// it).
fn push_import(
    out: &mut Vec<ImportEntry>,
    scope: &str,
    path: &str,
    alias: &str,
    is_glob: bool,
    import_stmt: Node,
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
        start_line: import_stmt.start_position().row as u64 + 1,
        end_line: import_stmt.end_position().row as u64 + 1,
    });
}

static TS_CONVENTIONS: TypeScriptConventions = TypeScriptConventions;

/// The TSX dialect: JSX syntax (`<Component/>`) exists ONLY in the `tsx`
/// grammar, so parsing a `.tsx`/`.jsx` file with the `typescript` grammar makes
/// every JSX element an ERROR node and drops the symbols inside it. The JS
/// family (`.js`/`.mjs`/`.cjs`) is routed to `Language::TypeScript` too and
/// carries no type syntax, so `tsx` is safe — and necessary for `.jsx`.
/// source: tree-sitter-typescript 0.23.2 (tsx/src/node-types.json carries the
/// `jsx_*` kinds the typescript grammar lacks); cross-ref GitNexus
/// parser-loader.ts, which selects the `:tsx` variant for .tsx.
static TSX_DIALECT: DialectSpec = DialectSpec {
    extensions: &["tsx", "jsx", "js", "mjs", "cjs"],
    ts_language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
};

/// The ECMAScript-family structural sub-table (ADR-0055 phase 7). All node kinds
/// and field names: tree-sitter-typescript 0.23.2 node-types.json, validated
/// against BOTH dialects by the spec guard.
static TS_FAMILY: TsFamilySpec = TsFamilySpec {
    func_decl_kinds: &["function_declaration", "generator_function_declaration"],
    class_decl_kinds: &["class_declaration", "abstract_class_declaration"],
    interface_decl_kinds: &["interface_declaration"],
    enum_decl_kinds: &["enum_declaration"],
    type_alias_kinds: &["type_alias_declaration"],
    value_decl_kinds: &["lexical_declaration", "variable_declaration"],
    export_stmt_kinds: &["export_statement"],
    class_body_kinds: &["class_body"],
    method_kinds: &["method_definition"],
    field_kinds: &["public_field_definition"],
    // Only `interface_body`: tree-sitter-typescript 0.23.2 constrains
    // `interface_declaration.body` to that single type. The hand-written walker
    // also accepted `object_type` here, which the grammar can never put in that
    // field — dead data, dropped rather than carried (§9).
    interface_body_kinds: &["interface_body"],
    method_signature_kinds: &["method_signature"],
    property_signature_kinds: &["property_signature"],
    enum_body_kinds: &["enum_body"],
    enum_member_kinds: &["enum_assignment"],
    enum_bare_member_kinds: &["property_identifier"],
    class_heritage_kinds: &["class_heritage"],
    extends_clause_kinds: &["extends_clause"],
    implements_clause_kinds: &["implements_clause"],
    interface_extends_kinds: &["extends_type_clause"],
    heritage_name_kinds: &["identifier", "type_identifier"],
    generic_type_kinds: &["generic_type"],
    declarator_kinds: &["variable_declarator"],
    arrow_func_kinds: &["arrow_function"],
    visibility_modifier_kinds: &["accessibility_modifier"],
    import_clause_kinds: &["import_clause"],
    named_imports_kinds: &["named_imports"],
    namespace_import_kinds: &["namespace_import"],
    import_specifier_kinds: &["import_specifier"],
    default_import_kinds: &["identifier"],
    value_field: "value",
    alias_field: "alias",
    import_source_field: "source",
};

/// The TypeScript language spec row. Class-model slices are empty; the `ts`
/// walker consumes `ts_family`. All node-kind strings: tree-sitter-typescript
/// 0.23.2 node-types.json (validated by
/// `guard::every_spec_node_kind_is_real_for_its_grammar` against both dialects).
pub(crate) static TS_SPEC: LangSpec = LangSpec {
    language: Language::TypeScript,
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
    // Unused by the `ts` walker (TypeScript has no generic `value_decl` path); a
    // real kind so the guard's unconditional `value_name_kind` check passes.
    value_name_kind: "identifier",
    variable_declarator_kind: None,
    import_node_kinds: &["import_statement"],
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
    ts_language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    embedded: &[],
    conventions: &TS_CONVENTIONS,
    c_family: None,
    cpp_family: None,
    objc_family: None,
    ts_family: Some(&TS_FAMILY),
    dialect: Some(&TSX_DIALECT),
};
