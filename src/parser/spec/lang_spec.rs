// parser::spec::lang_spec — the table-driven extraction schema (ADR-0055).
//
// A `LangSpec` is *pure data*: it names, per structural concern, the
// tree-sitter node kinds a grammar uses, plus the field names the generic
// walkers read and a grammar factory. Behavioral rules (visibility, QN
// shaping, call-callee extraction, import shaping) live in the companion
// `LanguageConventions` trait — the honest "data for structure, a small trait
// for behavior" split argued in ADR-0055 §4.
//
// Every string in every `*_node_kinds` / `*_kind` slice traces to that
// grammar's `node-types.json` (the source cited on the hand-written
// `TS_*` constants this table replaces). The spec-validation guard
// (`tests/spec_node_kinds.rs`) makes that citation executable: it loads each
// grammar's `NODE_TYPES` and asserts every kind in this table is real, turning
// the silent-drop failure mode of a stale row into a loud test failure
// (ADR-0055 Consequences, "the single most important guard in this ADR").

use tree_sitter::Language as TsLanguage;

use super::conventions::LanguageConventions;
use crate::parser::Language;

/// An embedded-language re-parse rule for host grammars (Vue/Svelte/Astro)
/// that leave an embedded language's source unparsed inside a host node.
///
/// The generic embedded walker locates each `script_node_kind` in the host
/// AST, takes its `content_node_kind` child's byte slice, re-parses it with
/// `embedded_language`'s grammar, and runs the same generic walkers on the
/// inner tree. Empty for all ten core languages (ADR-0055 §3): no core
/// grammar is a host grammar. Consumed by `walk_embedded` and validated by
/// the spec guard.
pub(crate) struct EmbeddedSpec {
    /// Host node kind carrying an embedded region (e.g. `script_element`).
    pub script_node_kind: &'static str,
    /// Child node kind of `script_node_kind` holding the raw embedded source.
    pub content_node_kind: &'static str,
    /// Language the raw content must be re-parsed as.
    pub embedded_language: Language,
}

/// Table-driven description of one language's structural node kinds.
///
/// Consumed by the generic walkers (`walk_defs` / `walk_calls` /
/// `walk_imports` / `walk_embedded`). Field-by-field consumption is asserted
/// by construction: no field exists that a walker or the spec guard does not
/// read (coding-standards §9 — no reserved dead fields).
pub(crate) struct LangSpec {
    /// Which language this row describes. Read by the registry and guard.
    pub language: Language,

    // --- structural node-kind slices (validated against node-types.json) ---
    /// Top-level child kinds to ignore outright (e.g. Go `package_clause`).
    pub skip_node_kinds: &'static [&'static str],
    /// Free-function declaration kinds → `Function` + `Defines`. When such a
    /// node is walked inside a `class_node_kinds` body (see below) it becomes a
    /// `Method` + `HasMethod` instead — the language distinguishes methods by
    /// enclosing scope (Python) rather than by a distinct node kind (Go, which
    /// leaves `class_node_kinds` empty and uses `method_node_kinds`).
    pub function_node_kinds: &'static [&'static str],
    /// Method declaration kinds → `Method` + `HasMethod` (receiver-scoped via
    /// `receiver_field`; Go `method_declaration`). Empty for languages whose
    /// methods are free-function nodes inside a class body (Python).
    pub method_node_kinds: &'static [&'static str],
    /// Class-like declaration kinds that map to `Struct`, may carry base
    /// classes (`extends_field`), and recurse into their `body_field` with the
    /// class as the enclosing scope (Python `class_definition`; Java
    /// `class_declaration`/`record_declaration`). Empty for languages without
    /// class-body recursion (Go, which uses type specs).
    pub class_node_kinds: &'static [&'static str],
    /// Class-like declaration kinds that map to `Trait` instead of `Struct`
    /// (Java `interface_declaration`/`annotation_type_declaration`). Walked by
    /// the same class machinery (inheritance + body recursion) as
    /// `class_node_kinds`, only the emitted label differs. Empty for languages
    /// that surface interfaces as type specs (Go) or have none (Python).
    pub interface_node_kinds: &'static [&'static str],
    /// Class-like declaration kinds that map to `Enum` (Java
    /// `enum_declaration`). Same class machinery, `Enum` label; their
    /// `variant_node_kinds` children become `Variant`s. Empty otherwise.
    pub enum_node_kinds: &'static [&'static str],
    /// Enum-member kinds inside an enum body → `Variant` + `HasVariant`
    /// (Java `enum_constant`). Empty for languages without enum variants.
    pub variant_node_kinds: &'static [&'static str],
    /// Member kinds emitted as a `Constant` + `Defines` under the current
    /// scope, whose name/visibility/properties are shaped by the conventions'
    /// `member_constant` (Kotlin `enum_entry` → `Constant` with an
    /// `enum_entry=true` property, NOT a `Variant`). Distinct from
    /// `variant_node_kinds` (which emits `Variant`/`HasVariant`) and from
    /// `variable_field_kinds` (Java class fields). Empty for languages whose
    /// enum members are `Variant`s or which have no such member (Go/Python/Java).
    pub member_constant_kinds: &'static [&'static str],
    /// Wrapper kinds carrying decorators plus a single inner def
    /// (Python `decorated_definition`). Empty for languages without decorators.
    pub decorated_def_kinds: &'static [&'static str],
    /// The decorator child kind inside a `decorated_def_kinds` node
    /// (Python `decorator`). `None` when the language has no decorators.
    pub decorator_node_kind: Option<&'static str>,
    /// Child kinds under `extends_field` naming a base class
    /// (Python `identifier`/`attribute`) → `Extends`. Empty for non-OO langs.
    pub base_node_kinds: &'static [&'static str],
    /// Wrapper kinds that contain a type spec (Go `type_declaration`).
    pub type_decl_node_kinds: &'static [&'static str],
    /// The named-type spec kinds inside a wrapper (Go `type_spec`/`type_alias`).
    pub type_spec_node_kinds: &'static [&'static str],
    /// Child `type_field` kind that marks a spec as a struct → `Struct`.
    pub struct_type_kind: Option<&'static str>,
    /// Child `type_field` kind that marks a spec as an interface → `Trait`.
    pub interface_type_kind: Option<&'static str>,
    /// Container kinds holding field declarations (Go `field_declaration_list`).
    pub field_container_kinds: &'static [&'static str],
    /// Field declaration kinds → `Field` + `HasField`.
    pub field_node_kinds: &'static [&'static str],
    /// Const/var declaration or statement kinds. Go `const_declaration`/
    /// `var_declaration`; Python `expression_statement` (module-level).
    /// Only walked at module scope (never inside a class body).
    pub value_decl_node_kinds: &'static [&'static str],
    /// Value-spec kinds inside a value decl. Go `const_spec`/`var_spec`;
    /// Python `assignment`.
    pub value_spec_node_kinds: &'static [&'static str],
    /// Leaf identifier kind naming a value (Go/Python `identifier`) → `Constant`.
    pub value_name_kind: &'static str,
    /// Declarator kind inside a `variable_field_kinds` node whose `name_field`
    /// names one declared member (Java `variable_declarator`; a single
    /// `field_declaration` may declare several). `None` for languages without
    /// `variable_field_kinds`.
    pub variable_declarator_kind: Option<&'static str>,
    /// Member-field declaration kinds emitted as `Constant` + `Defines` under
    /// the enclosing type (Java `field_declaration`, whose members are class
    /// fields the graph models as constants). Each declared name in a
    /// `variable_declarator_kind` child is one constant. Distinct from
    /// `value_decl_node_kinds` (module-scope const/var) and from
    /// `field_node_kinds` (Go struct fields → `Field`/`HasField`). Empty for
    /// languages without class-member fields (Go, Python).
    pub variable_field_kinds: &'static [&'static str],
    /// Kinds walked transparently with the current scope + enclosing type,
    /// without emitting a node — a grammar wrapper around further members
    /// (Java `enum_body_declarations`, which holds the methods/fields after an
    /// enum's constants). Empty for languages without such wrappers.
    pub body_wrapper_kinds: &'static [&'static str],
    /// Child node kinds that hold a class-like body to recurse into, for
    /// grammars whose body is a CHILD node rather than a named `body_field`
    /// (Kotlin `class_body`/`enum_class_body`; tree-sitter-kotlin-ng exposes no
    /// `body` field). When non-empty, `emit_class` recurses into the first child
    /// of one of these kinds instead of `body_field`. Empty for languages whose
    /// class body is a named field (Go/Python/Java).
    pub class_body_kinds: &'static [&'static str],
    /// Child node kinds that hold a function/method body to scan for calls, for
    /// grammars whose body is a CHILD node rather than a named `body_field`
    /// (Kotlin `function_body`). When non-empty, `emit_def` scans the first
    /// child of one of these kinds, falling back to the whole declaration node
    /// (so expression-bodied `fun f() = g()` still yields its call). Empty for
    /// languages whose function body is a named field (Go/Python/Java), which
    /// scan `body_field` and nothing when it is absent (abstract methods).
    pub function_body_kinds: &'static [&'static str],
    /// Import statement kinds (Go `import_declaration`; Python
    /// `import_statement`/`import_from_statement`/`future_import_statement`;
    /// Java `import_declaration`).
    pub import_node_kinds: &'static [&'static str],
    /// Import spec kinds inside an import statement (Go `import_spec`). May be
    /// empty for languages whose `imports_of` reads statement children directly.
    pub import_spec_kinds: &'static [&'static str],
    /// Call expression kinds (Go `call_expression`; Python `call`).
    pub call_node_kinds: &'static [&'static str],

    // --- field names the walkers read (validated against node-types fields) ---
    /// Field carrying a declaration's name (usually `name`).
    pub name_field: &'static str,
    /// Field carrying a function/method/class body (usually `body`). `None` for
    /// grammars that expose bodies as child NODES rather than a named field
    /// (Kotlin — see `class_body_kinds` / `function_body_kinds`), which have no
    /// `body` field to validate against `node-types.json`.
    pub body_field: Option<&'static str>,
    /// Field carrying a type / type-annotation (Go/Python `type`).
    pub type_field: &'static str,
    /// Field carrying a method receiver (Go `receiver`). `None` when the
    /// language has no receiver concept (Python — methods scope by class).
    pub receiver_field: Option<&'static str>,
    /// Field carrying an import path (Go `path`). `None` when `imports_of`
    /// reads the statement structure directly (Python).
    pub import_path_field: Option<&'static str>,
    /// Field naming the superclass list on a class node (Python `superclasses`).
    /// `None` for non-OO languages.
    pub extends_field: Option<&'static str>,
    /// Field on a value-spec node carrying the value's name. `Some` (Python
    /// `left`) selects the single-name, field-based path; `None` (Go) selects
    /// the multi-name DFS over `value_name_kind` children.
    pub value_name_field: Option<&'static str>,
    /// Field on a value-spec node carrying the value's type annotation
    /// (Python `type`). `Some` emits a `type_annotation` property on constants;
    /// `None` (Go) emits none.
    pub value_type_field: Option<&'static str>,

    // --- grammar factory, embedded rules, behavioral escape hatch ---
    /// Grammar factory (the Rust tree-sitter crate's `LANGUAGE`).
    pub ts_language: fn() -> TsLanguage,
    /// Embedded re-parse rules; empty for all core languages (ADR-0055 §3).
    pub embedded: &'static [EmbeddedSpec],
    /// Behavioral predicates and QN/entry shaping (ADR-0055 §4).
    pub conventions: &'static dyn LanguageConventions,
}
