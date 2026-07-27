// parser::spec::java — the Java LangSpec row + JavaConventions (ADR-0055
// phase 3). Java is the first migrated language whose type system carries the
// full OO spread the spec model must express structurally: interfaces and
// annotations (→ `Trait`), enums with constants (→ `Enum` + `Variant`),
// records (→ `Struct`), class-member fields (→ `Constant`), and — the two
// genuinely behavioral divergences — modifier-keyword visibility
// (`public`/`private`/`protected`, not a name convention) and a SPLIT
// inheritance model (`extends` one superclass vs `implements` an interface
// list, with different property keys and edge kinds). The structural spread is
// data (the spec row); the two behavioral divergences are this override. Its
// size is the ADR Risk-1 watch signal recorded in the PR.
//
// Every node-kind string in JAVA_SPEC is a real kind in tree-sitter-java's
// node-types.json (the spec-validation guard asserts it).
// source: tree-sitter-java 0.23.5 src/node-types.json
// (github.com/tree-sitter/tree-sitter-java, pinned in Cargo.lock).

use tree_sitter::Node;

use super::conventions::{CallEntry, ClassInheritance, ImportEntry, LanguageConventions};
use super::lang_spec::LangSpec;
use crate::parser::{node_field_text, node_text, qual, Language};

// Java-grammar node kinds / fields read only by the conventions (the
// structural ones are in JAVA_SPEC and validated by the guard).
// source: tree-sitter-java 0.23.5 node-types.json.
const JAVA_MODIFIERS: &str = "modifiers";
const JAVA_CALL_NAME_FIELD: &str = "name";
const JAVA_CALL_TYPE_FIELD: &str = "type";
const JAVA_SUPERCLASS_FIELD: &str = "superclass";
const JAVA_INTERFACES_FIELD: &str = "interfaces";
const JAVA_TYPE_IDENTIFIER: &str = "type_identifier";
const JAVA_SCOPED_TYPE_IDENTIFIER: &str = "scoped_type_identifier";
const JAVA_TYPE_LIST: &str = "type_list";

/// Java behavioral conventions — the ADR Risk-1 watch surface for phase 3.
pub(super) struct JavaConventions;

impl JavaConventions {
    /// Visibility from a declaration node's `modifiers` child. Java's access is
    /// a keyword on the declaration (`public`/`private`/`protected`), not a
    /// property of the name, so it must read the node. Absent modifier ⇒
    /// package-private, Java's default. source: JLS §6.6 (access control).
    fn visibility_from_modifiers(source: &str, node: Node) -> String {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == JAVA_MODIFIERS {
                let t = node_text(source, child);
                if t.contains("public") {
                    return "public".to_string();
                }
                if t.contains("private") {
                    return "private".to_string();
                }
                if t.contains("protected") {
                    return "protected".to_string();
                }
            }
        }
        "package".to_string()
    }

    /// The `extends` superclass name for a class (single; empty if none). The
    /// `superclass` field text is `extends Foo`; strip the keyword.
    fn extract_superclass(source: &str, node: Node) -> String {
        match node.child_by_field_name(JAVA_SUPERCLASS_FIELD) {
            Some(supers) => node_text(source, supers)
                .trim_start_matches("extends")
                .trim()
                .to_string(),
            None => String::new(),
        }
    }

    /// The implemented-interface names for a class (empty if none). The
    /// `interfaces` field is a `super_interfaces` node holding the `implements`
    /// keyword plus a `type_list`; the type names live one level down inside
    /// that `type_list`, so descend into it.
    fn extract_interfaces(source: &str, node: Node) -> Vec<String> {
        let ifaces = match node.child_by_field_name(JAVA_INTERFACES_FIELD) {
            Some(i) => i,
            None => return Vec::new(),
        };
        let mut names = Vec::new();
        Self::collect_type_names(source, ifaces, &mut names);
        names
    }

    /// Collects `type_identifier`/`scoped_type_identifier` names directly under
    /// `node`, descending one level through a `type_list` wrapper (the shape
    /// tree-sitter-java uses for `implements`/`extends`-interfaces clauses).
    fn collect_type_names(source: &str, node: Node, out: &mut Vec<String>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                JAVA_TYPE_IDENTIFIER | JAVA_SCOPED_TYPE_IDENTIFIER => {
                    let nm = node_text(source, child);
                    if !nm.is_empty() {
                        out.push(nm);
                    }
                }
                JAVA_TYPE_LIST => Self::collect_type_names(source, child, out),
                _ => {}
            }
        }
    }
}

impl LanguageConventions for JavaConventions {
    fn visibility_of(&self, _name: &str) -> String {
        // Java visibility is node-based (see `node_visibility`), so this
        // name-only entry is never reached for Java: every emitter that
        // computes a Java node's visibility goes through `node_visibility`, and
        // `constant_visibility` (the only other `visibility_of` caller) fires
        // only for `value_decl_node_kinds`, which Java leaves empty. The
        // package-private default is returned for the trait obligation.
        // mutation note (§12): a mutant of this return value is EQUIVALENT for
        // Java — no fixture can reach it (§9: no test for an unreachable path).
        "package".to_string()
    }

    fn receiver_type(&self, _receiver_text: &str) -> String {
        // Java methods scope by their enclosing type (context-based, via
        // `function_node_kinds` + `walk_defs`' `enclosing_class`), not by a
        // receiver node; `method_node_kinds` is empty, so `emit_method_recv` —
        // the only caller — never fires for Java.
        // mutation note (§12): EQUIVALENT for Java (unreachable path, §9).
        String::new()
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        // Overloaded methods share `scope::name`; the per-file `#seq` suffix
        // makes each definition's primary key unique (matching the pre-migration
        // walker), so the collision dedup is a no-op.
        format!("{scope}::{name}#{seq}")
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        // `method_invocation` carries the callee in its `name` field; an
        // `object_creation_expression` (`new Foo()`) carries the class in its
        // `type` field instead. Prefer `name`, fall back to `type`.
        let name = node_field_text(source, call_node, JAVA_CALL_NAME_FIELD);
        let callee = if name.is_empty() {
            node_field_text(source, call_node, JAVA_CALL_TYPE_FIELD)
        } else {
            name
        };
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
        seq: u64,
    ) -> CallEntry {
        let line = call_node.start_position().row + 1;
        let col = call_node.start_position().column + 1;
        CallEntry {
            name: callee.to_string(),
            qualified_name: format!("{caller_qn}::call@{line}:{col}#{seq}"),
            visibility: "package".to_string(),
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
        // `import a.b.C;` or `import static a.b.C.method;`. One statement is one
        // import; strip the keywords and trailing `;`, keep the dotted path
        // verbatim (the resolver keys on the last segment).
        let text = node_text(source, import_stmt);
        let cleaned = text
            .trim()
            .trim_start_matches("import")
            .trim()
            .trim_start_matches("static")
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string();
        if cleaned.is_empty() {
            return Vec::new();
        }
        let display_name = cleaned.rsplit('.').next().unwrap_or("").to_string();
        vec![ImportEntry {
            display_name,
            qualified_name: qual(scope, &format!("import:{cleaned}")),
            ref_to: cleaned.clone(),
            properties: vec![("path".to_string(), cleaned)],
            visibility: "package".to_string(),
            start_line: import_stmt.start_position().row as u64 + 1,
            end_line: import_stmt.end_position().row as u64 + 1,
        }]
    }

    fn node_visibility(&self, source: &str, node: Node, _name: &str) -> String {
        Self::visibility_from_modifiers(source, node)
    }

    fn class_inheritance(&self, source: &str, _spec: &LangSpec, node: Node) -> ClassInheritance {
        // Java splits inheritance in two: at most one `extends` superclass
        // (→ `bases` property + `Extends` ref) and any number of `implements`
        // interfaces (→ `implements` property + `Implements` refs). A property
        // is recorded only when its clause is present (matching the
        // pre-migration walker; Python, by contrast, always records `bases`).
        // Ref targets are kept verbatim (the resolver normalizes on lookup).
        let superclass = Self::extract_superclass(source, node);
        let interfaces = Self::extract_interfaces(source, node);

        let mut properties = Vec::new();
        if !superclass.is_empty() {
            properties.push(("bases".to_string(), superclass.clone()));
        }
        if !interfaces.is_empty() {
            properties.push(("implements".to_string(), interfaces.join(",")));
        }

        let mut refs: Vec<(&'static str, String)> = Vec::new();
        if !superclass.is_empty() {
            refs.push(("Extends", superclass));
        }
        for iface in interfaces {
            refs.push(("Implements", iface));
        }
        ClassInheritance { properties, refs }
    }
}

static JAVA_CONVENTIONS: JavaConventions = JavaConventions;

/// The Java language spec row. All node-kind strings: tree-sitter-java 0.23.5
/// node-types.json (validated by the spec guard). Java populates the OO
/// structural fields (`interface_node_kinds`, `enum_node_kinds`,
/// `variant_node_kinds`, `variable_field_kinds`, `body_wrapper_kinds`) and
/// leaves the Go/Python-specific fields (receiver, type specs, decorators,
/// module-level value decls, `extends_field`) empty/`None` — Java's inheritance
/// is handled by the `class_inheritance` override, not `extends_field`.
pub(crate) static JAVA_SPEC: LangSpec = LangSpec {
    language: Language::Java,
    skip_node_kinds: &["package_declaration"],
    function_node_kinds: &["method_declaration", "constructor_declaration"],
    method_node_kinds: &[],
    class_node_kinds: &["class_declaration", "record_declaration"],
    interface_node_kinds: &["interface_declaration", "annotation_type_declaration"],
    enum_node_kinds: &["enum_declaration"],
    variant_node_kinds: &["enum_constant"],
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
    variable_field_kinds: &["field_declaration"],
    body_wrapper_kinds: &["enum_body_declarations"],
    class_body_kinds: &[],
    function_body_kinds: &[],
    value_decl_node_kinds: &[],
    value_spec_node_kinds: &[],
    value_name_kind: "identifier",
    variable_declarator_kind: Some("variable_declarator"),
    import_node_kinds: &["import_declaration"],
    import_spec_kinds: &[],
    call_node_kinds: &["method_invocation", "object_creation_expression"],
    name_field: "name",
    body_field: Some("body"),
    type_field: "type",
    receiver_field: None,
    import_path_field: None,
    extends_field: None,
    value_name_field: None,
    value_type_field: None,
    ts_language: || tree_sitter_java::LANGUAGE.into(),
    embedded: &[],
    conventions: &JAVA_CONVENTIONS,
    c_family: None,
    cpp_family: None,
    objc_family: None,
    ts_family: None,
    ts_language_by_ext: None,
};
