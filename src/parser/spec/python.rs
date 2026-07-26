// parser::spec::python — the Python LangSpec row + PythonConventions
// (ADR-0055 phase 2). Python is the "richer-than-CBM" language the ADR's
// Risk #1 names: its extraction computes underscore visibility, UPPER_SNAKE
// constant detection, async flags, decorator CSVs, `@property`/`@setter` QN
// dedup, and a fundamentally different import structure (import / import-from /
// future, with dotted / aliased / wildcard children). None of that reduces to
// node-kind slices, so it lives here — in the behavioral trait — while the
// structural node kinds live in the spec row. The size of THIS impl is the
// Risk-1 watch signal recorded in the PR (the honest measure of how far the
// data/behavior split stretches for a rich language).
//
// Every node-kind string in PYTHON_SPEC is a real kind in tree-sitter-python's
// node-types.json (the spec-validation guard asserts it).
// source: tree-sitter-python 0.23.x src/node-types.json
// (github.com/tree-sitter/tree-sitter-python, pinned in Cargo.lock).

use tree_sitter::Node;

use super::conventions::{CallEntry, ImportEntry, LanguageConventions};
use super::lang_spec::LangSpec;
use crate::parser::{node_text, qual, Language};

// Python-grammar node kinds / fields read only by the conventions (the
// structural ones are in PYTHON_SPEC and validated by the guard).
// source: tree-sitter-python 0.23.x node-types.json.
const PY_IMPORT_STATEMENT: &str = "import_statement";
const PY_IMPORT_FROM_STATEMENT: &str = "import_from_statement";
const PY_FUTURE_IMPORT_STATEMENT: &str = "future_import_statement";
const PY_DOTTED_NAME: &str = "dotted_name";
const PY_IDENTIFIER: &str = "identifier";
const PY_ALIASED_IMPORT: &str = "aliased_import";
const PY_WILDCARD_IMPORT: &str = "wildcard_import";
const PY_CALL_FUNCTION_FIELD: &str = "function";
const PY_IMPORT_MODULE_FIELD: &str = "module_name";
const PY_ALIAS_NAME_FIELD: &str = "name";
const PY_ALIAS_ALIAS_FIELD: &str = "alias";
const PY_FUTURE_MODULE: &str = "__future__";

/// Python visibility convention: a leading underscore marks a name private,
/// except dunder names (`__init__`, `__str__`, …) which are public by
/// convention. source: PEP 8 (name mangling / non-public conventions).
pub(super) fn python_visibility(name: &str) -> String {
    if name.starts_with("__") && name.ends_with("__") {
        return String::new();
    }
    if name.starts_with('_') {
        "private".to_string()
    } else {
        String::new()
    }
}

/// Whether `name` is `UPPER_SNAKE_CASE` — the Python module-constant
/// convention. source: PEP 8 ("Constants are usually defined … in all capital
/// letters with underscores separating words").
pub(super) fn is_upper_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
        && name.chars().any(|c| c.is_ascii_uppercase())
}

/// Detects `async def` by checking whether the `function_definition` node's
/// text begins with the `async ` keyword (tree-sitter-python keeps async
/// functions as `function_definition`, prefixed by the `async` token).
fn is_async_function(source: &str, node: Node) -> bool {
    source[node.byte_range()].starts_with("async ")
}

/// Python behavioral conventions — the ADR Risk-1 watch surface.
pub(super) struct PythonConventions;

impl PythonConventions {
    /// Shapes one Python import into an `ImportEntry`, or `None` for an empty
    /// path. `is_glob` renders the display as `{path}::*`; a non-empty `alias`
    /// wins as the display name. The import ref is a file-local `Defines`
    /// (`ref_to` is the node's own QN), so `line`s come from the statement
    /// `node`.
    #[allow(clippy::too_many_arguments)]
    fn py_import_entry(
        &self,
        scope: &str,
        path: String,
        alias: String,
        is_glob: bool,
        node: Node,
    ) -> Option<ImportEntry> {
        if path.is_empty() {
            return None;
        }
        let display_name = if !alias.is_empty() {
            alias.clone()
        } else if is_glob {
            format!("{path}::*")
        } else {
            path.clone()
        };
        let qn = qual(scope, &display_name);
        Some(ImportEntry {
            display_name,
            qualified_name: qn.clone(),
            ref_to: qn,
            properties: vec![
                ("path".to_string(), path),
                ("alias".to_string(), alias),
                ("is_glob".to_string(), is_glob.to_string()),
            ],
            visibility: String::new(),
            start_line: node.start_position().row as u64 + 1,
            end_line: node.end_position().row as u64 + 1,
        })
    }

    /// `import foo` / `import foo.bar` / `import foo as f`.
    fn imports_plain(&self, source: &str, node: Node, scope: &str) -> Vec<ImportEntry> {
        let mut out = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == PY_DOTTED_NAME {
                let path = node_text(source, child).replace('.', "::");
                if let Some(e) = self.py_import_entry(scope, path, String::new(), false, node) {
                    out.push(e);
                }
            } else if child.kind() == PY_ALIASED_IMPORT {
                let path = child
                    .child_by_field_name(PY_ALIAS_NAME_FIELD)
                    .map(|n| node_text(source, n).replace('.', "::"))
                    .unwrap_or_default();
                let alias = child
                    .child_by_field_name(PY_ALIAS_ALIAS_FIELD)
                    .map(|n| node_text(source, n))
                    .unwrap_or_default();
                if let Some(e) = self.py_import_entry(scope, path, alias, false, node) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// `from foo import bar` / `from foo import *` / `from foo import bar as b`.
    fn imports_from(&self, source: &str, node: Node, scope: &str) -> Vec<ImportEntry> {
        let module_name = node
            .child_by_field_name(PY_IMPORT_MODULE_FIELD)
            .map(|n| node_text(source, n).replace('.', "::"))
            .unwrap_or_default();

        // Wildcard short-circuits: `from foo import *` yields a single glob.
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == PY_WILDCARD_IMPORT {
                return self
                    .py_import_entry(scope, module_name, String::new(), true, node)
                    .into_iter()
                    .collect();
            }
        }

        let module_id = node
            .child_by_field_name(PY_IMPORT_MODULE_FIELD)
            .map(|n| n.id());
        let full_path = |name: String| -> String {
            if module_name.is_empty() {
                name
            } else {
                format!("{module_name}::{name}")
            }
        };
        let mut out = Vec::new();
        let mut cursor2 = node.walk();
        for child in node.children(&mut cursor2) {
            if child.kind() == PY_DOTTED_NAME || child.kind() == PY_IDENTIFIER {
                // Skip the module-name node itself.
                if Some(child.id()) == module_id {
                    continue;
                }
                let path = full_path(node_text(source, child));
                if let Some(e) = self.py_import_entry(scope, path, String::new(), false, node) {
                    out.push(e);
                }
            } else if child.kind() == PY_ALIASED_IMPORT {
                let name = child
                    .child_by_field_name(PY_ALIAS_NAME_FIELD)
                    .map(|n| node_text(source, n))
                    .unwrap_or_default();
                let alias = child
                    .child_by_field_name(PY_ALIAS_ALIAS_FIELD)
                    .map(|n| node_text(source, n))
                    .unwrap_or_default();
                let path = full_path(name);
                if let Some(e) = self.py_import_entry(scope, path, alias, false, node) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// `from __future__ import X [as Y][, Z …]`. tree-sitter-python gives this
    /// its own node kind; the module is implicitly `__future__`.
    fn imports_future(&self, source: &str, node: Node, scope: &str) -> Vec<ImportEntry> {
        let mut out = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                PY_IDENTIFIER | PY_DOTTED_NAME => {
                    let name = node_text(source, child);
                    // Skip the literal `__future__` token if the grammar emits
                    // it as an identifier child (grammar-version defensive;
                    // ported verbatim for parity).
                    // mutation note (§12): the `||` → `&&` mutant here is
                    // EQUIVALENT under the pinned grammar — a `dotted_name`/
                    // `identifier` import child is never empty and never the
                    // literal `__future__` (that token is not emitted as a
                    // child), so both operands are always false and the guard
                    // never fires either way. Kept for parity + forward-grammar
                    // safety; no fixture can observe the operator.
                    if name == PY_FUTURE_MODULE || name.is_empty() {
                        continue;
                    }
                    let path = format!("{PY_FUTURE_MODULE}::{name}");
                    if let Some(e) = self.py_import_entry(scope, path, String::new(), false, node) {
                        out.push(e);
                    }
                }
                PY_ALIASED_IMPORT => {
                    let name = child
                        .child_by_field_name(PY_ALIAS_NAME_FIELD)
                        .map(|n| node_text(source, n))
                        .unwrap_or_default();
                    let alias = child
                        .child_by_field_name(PY_ALIAS_ALIAS_FIELD)
                        .map(|n| node_text(source, n))
                        .unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    let path = format!("{PY_FUTURE_MODULE}::{name}");
                    if let Some(e) = self.py_import_entry(scope, path, alias, false, node) {
                        out.push(e);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

impl LanguageConventions for PythonConventions {
    fn visibility_of(&self, name: &str) -> String {
        python_visibility(name)
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // Python has no receiver node; methods scope by their enclosing class
        // (handled by `walk_defs` via `enclosing_class`), so this is never
        // called for Python (`method_node_kinds` is empty).
        // mutation note (§12): any mutant of this return value is EQUIVALENT for
        // Python — `receiver_type` is reached only from `emit_method_recv`,
        // which fires only for `method_node_kinds`, and Python's is empty. The
        // method exists solely to satisfy the trait Go shares. No test can (or
        // should — §9) exercise an unreachable path.
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, _seq: u64) -> String {
        // Python disambiguates overloads by the walker's collision dedup
        // (`@line`), not by a sequence suffix, so the base QN is plain.
        qual(scope, name)
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        let func = call_node.child_by_field_name(PY_CALL_FUNCTION_FIELD)?;
        let callee = node_text(source, func);
        if callee.is_empty() {
            None
        } else {
            // Full callee text is kept verbatim (`self.foo`, `mod.func`); the
            // resolver decides whether it can resolve the target (Bug #10).
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
        // Column is 0-based here (matching the pre-migration walker's key).
        let col = call_node.start_position().column as u64;
        // Chained calls `f()()` share a start byte, so the (start,end) byte
        // span disambiguates every call while the line:col prefix stays human-
        // readable.
        let start_byte = call_node.start_byte() as u64;
        let end_byte = call_node.end_byte() as u64;
        let cs_id = format!("{caller_qn}::call@{line}:{col}#{start_byte}-{end_byte}");
        CallEntry {
            name: callee.to_string(),
            qualified_name: cs_id.clone(),
            visibility: String::new(),
            properties: vec![
                ("callee_name".to_string(), callee.to_string()),
                ("caller_qn".to_string(), caller_qn.to_string()),
            ],
            start_line: line,
            end_line: line,
            ref_kind: "Defines",
            ref_to: cs_id,
        }
    }

    fn imports_of(
        &self,
        source: &str,
        _spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        match import_stmt.kind() {
            PY_IMPORT_STATEMENT => self.imports_plain(source, import_stmt, scope),
            PY_IMPORT_FROM_STATEMENT => self.imports_from(source, import_stmt, scope),
            PY_FUTURE_IMPORT_STATEMENT => self.imports_future(source, import_stmt, scope),
            _ => Vec::new(),
        }
    }

    fn is_constant_name(&self, name: &str) -> bool {
        is_upper_snake_case(name)
    }

    fn function_props(&self, source: &str, node: Node) -> Vec<(String, String)> {
        vec![(
            "is_async".to_string(),
            is_async_function(source, node).to_string(),
        )]
    }

    fn constant_visibility(&self, _name: &str) -> String {
        // Module constants carry no visibility, even `_LEADING`/`UPPER` names.
        String::new()
    }

    fn import_ref_kind(&self) -> &'static str {
        // Python import refs are file-local `Defines` edges (the resolver
        // upgrades them to cross-file `Imports` in a later pass).
        "Defines"
    }
}

static PYTHON_CONVENTIONS: PythonConventions = PythonConventions;

/// The Python language spec row. All node-kind strings: tree-sitter-python
/// node-types.json (validated by the spec guard). Go-specific fields
/// (`method_node_kinds`, `type_decl_node_kinds`, receiver/import-path fields)
/// are empty/`None`; Python-specific fields (`class_node_kinds`, decorators,
/// `extends_field`, field-based constants) are populated.
pub(crate) static PYTHON_SPEC: LangSpec = LangSpec {
    language: Language::Python,
    skip_node_kinds: &[],
    function_node_kinds: &["function_definition"],
    method_node_kinds: &[],
    class_node_kinds: &["class_definition"],
    interface_node_kinds: &[],
    enum_node_kinds: &[],
    variant_node_kinds: &[],
    member_constant_kinds: &[],
    decorated_def_kinds: &["decorated_definition"],
    decorator_node_kind: Some("decorator"),
    base_node_kinds: &["identifier", "attribute"],
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
    value_decl_node_kinds: &["expression_statement"],
    value_spec_node_kinds: &["assignment"],
    value_name_kind: "identifier",
    variable_declarator_kind: None,
    import_node_kinds: &[
        "import_statement",
        "import_from_statement",
        "future_import_statement",
    ],
    import_spec_kinds: &[],
    call_node_kinds: &["call"],
    name_field: "name",
    body_field: Some("body"),
    type_field: "type",
    receiver_field: None,
    import_path_field: None,
    extends_field: Some("superclasses"),
    value_name_field: Some("left"),
    value_type_field: Some("type"),
    ts_language: || tree_sitter_python::LANGUAGE.into(),
    embedded: &[],
    conventions: &PYTHON_CONVENTIONS,
    c_family: None,
};

#[cfg(test)]
mod tests {
    // These pin the two behavioral predicates directly (moved here with the
    // predicates when the hand-written `parser::python` module was deleted).
    // The corpus parity test covers their common cases; these cover the edge
    // cases that make the `&&` in each observably load-bearing (§12): a
    // prefix-only dunder (`__mangled`) for visibility, and an all-underscore /
    // lowercase name for the constant filter.
    use super::{is_upper_snake_case, python_visibility};

    #[test]
    fn python_visibility_rules() {
        assert_eq!(python_visibility("public_func"), "");
        assert_eq!(python_visibility("_private_func"), "private");
        // Prefix-only dunder is NOT a dunder method → private (kills the
        // `starts_with("__") && ends_with("__")` → `||` mutant).
        assert_eq!(python_visibility("__mangled"), "private");
        // Real dunder methods are public by convention.
        assert_eq!(python_visibility("__init__"), "");
        assert_eq!(python_visibility("__str__"), "");
    }

    #[test]
    fn upper_snake_case_rules() {
        assert!(is_upper_snake_case("MAX_SIZE"));
        assert!(is_upper_snake_case("FOO"));
        assert!(is_upper_snake_case("HTTP_200"));
        assert!(!is_upper_snake_case("foo"));
        assert!(!is_upper_snake_case("Foo_Bar"));
        // All-underscore has no uppercase → NOT a constant (kills the
        // `all(...) && any(is_uppercase)` → `||` mutant).
        assert!(!is_upper_snake_case("_"));
        assert!(!is_upper_snake_case(""));
    }
}
