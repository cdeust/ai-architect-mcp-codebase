// parser::spec::java — the Java LangSpec row (ADR-0055 phase 3; issue #220
// phase 2: migrated to the declarative deep path). Java's type system carries
// the full OO spread the spec model must express structurally: interfaces and
// annotations (→ `Trait`), enums with constants (→ `Enum` + `Variant`),
// records (→ `Struct`), class-member fields (→ `Constant`), and — the two
// genuinely behavioral divergences — modifier-keyword visibility
// (`public`/`private`/`protected`, not a name convention) and a SPLIT
// inheritance model (`extends` one superclass vs `implements` an interface
// list, with different property keys and edge kinds).
//
// issue #220 phase 2: production now runs Java through
// `JavaDeclarativeConventions` (below) — all six required
// `LanguageConventions` methods, plus the *defaulted* `node_visibility`, are
// driven by `JAVA_RULES: ConventionSpec` through the shared
// `DeclarativeConventions` engine (`declarative.rs`), exactly as Go's phase 1
// migration. `class_inheritance` — an EIGHTH, also-defaulted trait method
// Java overrides for its extends/implements split — is NOT part of
// `ConventionSpec` (design comment §2 sketches a declarative
// `implements_field` extension but scopes it below the six required methods)
// and stays hand-written: an honest, METHOD-granularity escape hatch, not a
// whole-trait one. Folding it into a shared node-kind-matching mechanism
// risked a real parity break — `extract_superclass` keeps a superclass's
// generic type args verbatim by stripping the `extends` keyword from the
// WHOLE field text, not by walking `type_identifier`/`scoped_type_identifier`
// children the way Python's default `class_inheritance` does — a shape
// distinct from every proven `ConventionSpec` variant. See the PR body's
// "what needed the escape hatch" section.
//
// The hand-written `JavaConventions` below is KEPT, unused by `JAVA_SPEC`, as
// the rollback path and the parity oracle for
// `java_declarative_parity_tests.rs` (ADR rollback discipline — never deleted
// in the same PR that introduces its declarative row). The PRE-EXISTING
// `java_parity_tests.rs` (ADR-0055 walker→spec migration) is untouched and
// still pins the same corpus against a frozen ground-truth transcript — see
// `java_declarative_parity_tests.rs`'s module doc comment for why both are
// kept as independent oracles rather than merged.
//
// Every node-kind string in JAVA_SPEC is a real kind in tree-sitter-java's
// node-types.json (the spec-validation guard asserts it).
// source: tree-sitter-java 0.23.5 src/node-types.json
// (github.com/tree-sitter/tree-sitter-java, pinned in Cargo.lock).

use tree_sitter::Node;

use super::conventions::{CallEntry, ClassInheritance, ImportEntry, LanguageConventions};
use super::declarative::DeclarativeConventions;
use super::declarative_rules::{
    CallEntryRule, CallSiteQnScheme, CalleeDispatchRow, CalleeTransform, ConventionSpec,
    ImportRule, ModifierSource, PropertySet, QnScheme, RefToRule, VisibilityRule,
};
use super::lang_spec::LangSpec;
use crate::parser::{node_text, Language};

// Used only by the test-only `JavaConventions` (parity oracle) below.
#[cfg(test)]
use crate::parser::{node_field_text, qual};

// Java-grammar node kinds / fields read by the conventions (the structural
// ones are in JAVA_SPEC and validated by the guard).
// source: tree-sitter-java 0.23.5 node-types.json.
const JAVA_MODIFIERS: &str = "modifiers";
const JAVA_CALL_NAME_FIELD: &str = "name";
const JAVA_CALL_TYPE_FIELD: &str = "type";
const JAVA_SUPERCLASS_FIELD: &str = "superclass";
const JAVA_INTERFACES_FIELD: &str = "interfaces";
const JAVA_TYPE_IDENTIFIER: &str = "type_identifier";
const JAVA_SCOPED_TYPE_IDENTIFIER: &str = "scoped_type_identifier";
const JAVA_TYPE_LIST: &str = "type_list";

/// Java's six-required-method + `node_visibility` behavior as a
/// `ConventionSpec` data row (issue #220 phase 2). Every value below is read
/// directly off `JavaConventions` (this file, preserved below as the parity
/// oracle) — this row reproduces it exactly, proven by
/// `java_declarative_parity_tests.rs`.
pub(super) static JAVA_RULES: ConventionSpec = ConventionSpec {
    // JavaConventions::node_visibility / visibility_from_modifiers: scan the
    // declaration's immediate children for a `modifiers` child (never a named
    // field in tree-sitter-java's grammar — see `ModifierSource::ChildKind`'s
    // doc comment), first matching keyword wins, else "package"
    // (package-private, Java's default — JLS §6.6). `visibility_of` (name
    // only, no node) is unreachable for Java (every caller goes through
    // `node_visibility`); it returns `default_label`, matching
    // `JavaConventions::visibility_of`'s unconditional `"package"`.
    visibility: VisibilityRule::ModifierKeyword {
        modifier_source: ModifierSource::ChildKind(JAVA_MODIFIERS),
        candidates: &[
            ("public", "public"),
            ("private", "private"),
            ("protected", "protected"),
        ],
        default_label: "package",
    },
    // JavaConventions::receiver_type: unreachable for Java (methods scope by
    // enclosing type, not a receiver node; `method_node_kinds` is empty) —
    // `None` reproduces the unconditional empty-string return.
    receiver_pattern: None,
    // JavaConventions::def_qn: `{scope}::{name}#{seq}`.
    qn_scheme: QnScheme::SeqSuffixed,
    // JavaConventions::call_callee: `method_invocation` carries the callee in
    // its `name` field; `object_creation_expression` (`new Foo()`) carries it
    // in `type` instead — one wildcard-node-kind row with a fallback field,
    // kept verbatim (no separator splitting), only gated on non-empty (no
    // identifier-leading-char gate, unlike Go).
    callee_dispatch: &[CalleeDispatchRow {
        node_kind: None,
        field: JAVA_CALL_NAME_FIELD,
        fallback_field: Some(JAVA_CALL_TYPE_FIELD),
        transform: CalleeTransform::Verbatim,
        require_leading_ident: false,
        suffix: None,
    }],
    // JavaConventions::call_entry: `{caller}::call@{line}:{col}#{seq}`,
    // `Calls`, ref_to = the callee verbatim, `package` (NOT `public` — Java's
    // call-site visibility literal differs from Go's), `[callee_name]`.
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "package",
        properties: PropertySet::CalleeOnly,
    },
    // JavaConventions::imports_of: `import a.b.C;` / `import static
    // a.b.C.method;` — one statement is one import; strip the `import` and
    // `static` keywords (in that order, matching the original's two chained
    // `trim_start_matches` calls) and a trailing `;`, keep the dotted path
    // verbatim.
    import_rule: ImportRule::StatementStrip {
        strip_prefixes: &["import", "static"],
        trim_end: &[';'],
        delimiter_pair: None,
    },
};

static JAVA_DECLARATIVE_CONVENTIONS: DeclarativeConventions = DeclarativeConventions(&JAVA_RULES);

/// Production Java conventions (issue #220 phase 2): delegates every
/// `ConventionSpec`-expressible method to `JAVA_DECLARATIVE_CONVENTIONS`, and
/// hand-implements `class_inheritance` (the one method that does not yet fit
/// any `ConventionSpec` rule variant — see the module doc comment). An
/// honest, method-granularity escape hatch: six methods are data, one is
/// code, and which is which is visible at the call site, not hidden behind a
/// blanket "Java needs a hand-written impl" classification.
pub(super) struct JavaDeclarativeConventions;

impl LanguageConventions for JavaDeclarativeConventions {
    fn visibility_of(&self, name: &str) -> String {
        JAVA_DECLARATIVE_CONVENTIONS.visibility_of(name)
    }

    fn node_visibility(&self, source: &str, node: Node, name: &str) -> String {
        JAVA_DECLARATIVE_CONVENTIONS.node_visibility(source, node, name)
    }

    fn receiver_type(&self, receiver_text: &str) -> String {
        JAVA_DECLARATIVE_CONVENTIONS.receiver_type(receiver_text)
    }

    fn def_qn(&self, scope: &str, name: &str, seq: u64) -> String {
        JAVA_DECLARATIVE_CONVENTIONS.def_qn(scope, name, seq)
    }

    fn call_callee(&self, source: &str, call_node: Node) -> Option<String> {
        JAVA_DECLARATIVE_CONVENTIONS.call_callee(source, call_node)
    }

    fn call_entry(
        &self,
        source: &str,
        call_node: Node,
        caller_qn: &str,
        callee: &str,
        seq: u64,
    ) -> CallEntry {
        JAVA_DECLARATIVE_CONVENTIONS.call_entry(source, call_node, caller_qn, callee, seq)
    }

    fn imports_of(
        &self,
        source: &str,
        spec: &LangSpec,
        import_stmt: Node,
        scope: &str,
    ) -> Vec<ImportEntry> {
        JAVA_DECLARATIVE_CONVENTIONS.imports_of(source, spec, import_stmt, scope)
    }

    fn class_inheritance(&self, source: &str, spec: &LangSpec, node: Node) -> ClassInheritance {
        java_class_inheritance(source, spec, node)
    }
}

/// The `extends`/`implements` split-inheritance shape, shared verbatim
/// between the production `JavaDeclarativeConventions` and the test-only
/// parity-oracle `JavaConventions` below — one implementation, not two
/// hand-kept-in-sync copies. Java splits inheritance in two: at most one
/// `extends` superclass (→ `bases` property + `Extends` ref) and any number
/// of `implements` interfaces (→ `implements` property + `Implements` refs).
/// A property is recorded only when its clause is present (matching the
/// pre-migration walker; Python, by contrast, always records `bases`). Ref
/// targets are kept verbatim (the resolver normalizes on lookup).
fn java_class_inheritance(source: &str, _spec: &LangSpec, node: Node) -> ClassInheritance {
    let superclass = java_extract_superclass(source, node);
    let interfaces = java_extract_interfaces(source, node);

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

/// The `extends` superclass name for a class (single; empty if none). The
/// `superclass` field text is `extends Foo`; strip the keyword.
fn java_extract_superclass(source: &str, node: Node) -> String {
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
fn java_extract_interfaces(source: &str, node: Node) -> Vec<String> {
    let ifaces = match node.child_by_field_name(JAVA_INTERFACES_FIELD) {
        Some(i) => i,
        None => return Vec::new(),
    };
    let mut names = Vec::new();
    java_collect_type_names(source, ifaces, &mut names);
    names
}

/// Collects `type_identifier`/`scoped_type_identifier` names directly under
/// `node`, descending one level through a `type_list` wrapper (the shape
/// tree-sitter-java uses for `implements`/`extends`-interfaces clauses).
fn java_collect_type_names(source: &str, node: Node, out: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            JAVA_TYPE_IDENTIFIER | JAVA_SCOPED_TYPE_IDENTIFIER => {
                let nm = node_text(source, child);
                if !nm.is_empty() {
                    out.push(nm);
                }
            }
            JAVA_TYPE_LIST => java_collect_type_names(source, child, out),
            _ => {}
        }
    }
}

/// Java's ORIGINAL hand-written behavioral conventions (ADR-0055 phase 3).
/// Superseded in production by `JavaDeclarativeConventions` above (issue #220
/// phase 2) for every method except `class_inheritance` (reused via
/// `java_class_inheritance`, not duplicated). Gated `#[cfg(test)]`: its only
/// caller is `JAVA_SPEC_LEGACY` below, the `java_declarative_parity_tests.rs`
/// oracle. Kept, not deleted, per the ADR rollback discipline.
#[cfg(test)]
pub(super) struct JavaConventions;

#[cfg(test)]
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
}

#[cfg(test)]
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

    fn class_inheritance(&self, source: &str, spec: &LangSpec, node: Node) -> ClassInheritance {
        // Shared with `JavaDeclarativeConventions` — see
        // `java_class_inheritance`'s doc comment.
        java_class_inheritance(source, spec, node)
    }
}

#[cfg(test)]
static JAVA_CONVENTIONS: JavaConventions = JavaConventions;

static JAVA_DECLARATIVE_CONVENTIONS_WRAPPER: JavaDeclarativeConventions =
    JavaDeclarativeConventions;

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
    // Not adopted (issue #92): `new T()` is already an `object_creation_expression`
    // CallSite the resolver turns into a Uses edge.
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
    ts_language: || tree_sitter_java::LANGUAGE.into(),
    embedded: &[],
    // issue #220 phase 2: production runs the declarative-backed wrapper, not
    // the hand-written `JavaConventions` — see the module doc comment for the
    // rollback story.
    conventions: &JAVA_DECLARATIVE_CONVENTIONS_WRAPPER,
    c_family: None,
    cpp_family: None,
    objc_family: None,
    ts_family: None,
    ts_language_by_ext: None,
    rust_family: None,
};

/// Test-only twin of `JAVA_SPEC` wired to the hand-written `JavaConventions`
/// instead of the declarative wrapper — the parity oracle
/// `java_declarative_parity_tests.rs` parses the same corpus through both and
/// asserts identical output. Every field but `conventions` is copied verbatim
/// from `JAVA_SPEC` (all `LangSpec` fields are `Copy` types, so `..JAVA_SPEC`
/// is a plain field copy, not a move out of a `static`).
#[cfg(test)]
pub(crate) static JAVA_SPEC_LEGACY: LangSpec = LangSpec {
    conventions: &JAVA_CONVENTIONS,
    ..JAVA_SPEC
};
