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
// The per-family structural sub-tables live in `families` (§4.1 split); they are
// re-exported here so every consumer's `super::lang_spec::{CFamilySpec, …}`
// import — and this module's own `LangSpec` field types — resolve unchanged.
pub(crate) use super::families::{
    CFamilySpec, CppFamilySpec, DeclaratorNaming, EmbeddedSpec, ObjcFamilySpec, TsFamilySpec,
};
use crate::parser::Language;

// The per-family structural sub-tables (EmbeddedSpec, DeclaratorNaming,
// CFamilySpec, CppFamilySpec, ObjcFamilySpec, TsFamilySpec) live in
// `families.rs` (§4.1 split, #144) and are re-exported above. The #123/#124
// field additions landed in `families.rs`, not here.

/// The node kinds a **Rust** grammar uses (ADR-0055 phase 8).
///
/// Rust fits none of the three existing shapes. Its hand-written walker
/// (`parser::rust::extract`) is an item-list recursion whose divergences are
/// structural, not behavioral:
/// - `#[derive(...)]` attributes are **accumulated across siblings** and applied
///   to the next struct/enum/union as an `implements` property plus synthetic
///   `DeriveImplements` edges; a non-item child (a comment) does NOT reset the
///   accumulator.
/// - An `impl_item` emits **no node of its own**; its methods attach to
///   `{file_path}::{impl_type_text}` — the FILE scope, not the enclosing module,
///   and the type text carries its generics (`Wrapper<T>`).
/// - A `trait_item`'s requirements are `Method`s whose bodies are NOT scanned
///   for calls, while an `impl_item`'s method bodies are.
/// - `use` declarations expand a brace/alias/glob tree into one atomic `Import`
///   per leaf with a `Defines` edge, while `extern crate` emits an `Import` with
///   an `Imports` edge — one language, two import edge kinds.
/// - A `call_expression` emits the call site **plus** one extra call site per
///   function-value argument (issue #87), and a `macro_invocation` emits a call
///   site whose callee carries a trailing `!`.
///
/// When a `LangSpec` carries `rust_family: Some(_)`, `walk_defs` delegates the
/// whole file to `walkers/rust.rs` (the #109 `clike` / #125 `cpp` precedent),
/// leaving `walk_defs` / `clike` / `cpp` and the seven languages riding them
/// untouched. Calls and imports still route through the SHARED generic walkers
/// (`calls::walk_calls`, `imports::walk_imports`) via `RustConventions`.
///
/// Every string traces to tree-sitter-rust 0.23.3's `node-types.json` and is
/// validated by the spec guard.
/// source: tree-sitter-rust 0.23.3 src/node-types.json.
pub(crate) struct RustFamilySpec {
    /// Attribute kinds whose `#[derive(A, B)]` payload accumulates into the
    /// pending-derive list applied to the next struct/enum/union
    /// (`attribute_item`). Emits no node.
    pub attribute_kinds: &'static [&'static str],
    /// Free-function item kinds → `Function` + `Defines` at item scope, or
    /// `Method` + `HasMethod` inside a trait/impl body (`function_item`).
    pub function_kinds: &'static [&'static str],
    /// Bodiless function-declaration kinds, valid only inside a trait or impl
    /// body → `Method` + `HasMethod` with no call scan
    /// (`function_signature_item`).
    pub function_signature_kinds: &'static [&'static str],
    /// Struct-like item kinds → `Struct` + `Defines`, recursing the `body_field`
    /// for fields (`struct_item`, `union_item` — a union names and lists its
    /// members exactly as a struct does).
    pub struct_like_kinds: &'static [&'static str],
    /// Enum item kinds → `Enum` + `Defines`, whose `variant_kinds` body children
    /// become `Variant`s (`enum_item`).
    pub enum_kinds: &'static [&'static str],
    /// Enum-member kinds inside a `variant_list_kinds` body → `Variant` +
    /// `HasVariant` (`enum_variant`).
    pub variant_kinds: &'static [&'static str],
    /// The `body_field` kind an enum's variants live in (`enum_variant_list`).
    /// A body of any other kind yields no variants.
    pub variant_list_kinds: &'static [&'static str],
    /// Trait item kinds → `Trait` + `Defines`, supertraits → `Extends`, and
    /// requirements → `Method` + `HasMethod` (`trait_item`).
    pub trait_kinds: &'static [&'static str],
    /// Impl-block kinds — emit NO node; their methods attach to the impl type
    /// under the FILE scope (`impl_item`).
    pub impl_kinds: &'static [&'static str],
    /// The `body_field` kind a trait/impl/module body takes (`declaration_list`).
    /// A body of any other kind is not walked.
    pub decl_list_kinds: &'static [&'static str],
    /// Const/static item kinds → `Constant` + `Defines` with a `type_annotation`
    /// property (`const_item`, `static_item` — same name/type fields).
    pub constant_kinds: &'static [&'static str],
    /// `macro_rules!` definition kinds → `Constant` (`is_macro=true`) +
    /// `Defines`; AP has no dedicated Macro label (`macro_definition`).
    pub macro_def_kinds: &'static [&'static str],
    /// `extern crate` kinds → `Import` + an **`Imports`** edge to the crate name
    /// (`extern_crate_declaration`). Distinct from `use_kinds`, whose edge is
    /// `Defines`.
    pub extern_crate_kinds: &'static [&'static str],
    /// Type-alias item kinds → `TypeAlias` + `Defines` with a `target_type`
    /// property read from `type_field` (`type_item`).
    pub type_alias_kinds: &'static [&'static str],
    /// `use` declaration kinds → one `Import` + **`Defines`** edge per expanded
    /// leaf of the `argument_field` tree (`use_declaration`).
    pub use_kinds: &'static [&'static str],
    /// Module item kinds → `Module` + `Defines`, recursing a `decl_list_kinds`
    /// body under the module's QN (`mod_item`). A bodiless `mod foo;` emits the
    /// node and recurses nothing.
    pub mod_kinds: &'static [&'static str],
    /// Brace-list kind in a `use` tree whose named children are expanded under
    /// the current prefix (`use_list`).
    pub use_list_kind: &'static str,
    /// Prefixed brace-list kind in a `use` tree: `path_field` extends the prefix,
    /// `list_field` holds the leaves (`scoped_use_list`).
    pub scoped_use_list_kind: &'static str,
    /// Aliased-import kind in a `use` tree: `path_field` + `alias_field`
    /// (`use_as_clause`).
    pub use_as_clause_kind: &'static str,
    /// Glob kind in a `use` tree; its text is `<path>::*` (`use_wildcard`).
    pub use_wildcard_kind: &'static str,
    /// The call-site kind whose callee is a macro path rather than a callee
    /// expression (`macro_invocation`): the callee is `macro_field`'s text plus a
    /// trailing `!`, so the resolver's Layer 4 can tell a macro from a function
    /// without re-parsing. Also listed in `call_node_kinds` so the shared call
    /// DFS visits it.
    pub macro_invocation_kind: &'static str,
    /// Bare-callee kinds a call's `arguments_field` child may take to count as a
    /// function passed **by value** (issue #87) → one extra `CallSite` each
    /// (`identifier`, `scoped_identifier`). Anything else (a nested call, a
    /// closure, a reference, a literal) is not a function reference.
    pub fn_value_arg_kinds: &'static [&'static str],
    /// The child kind carrying a declaration's visibility; its verbatim text IS
    /// the visibility (`pub`, `pub(crate)`, `pub(super)`), and its absence means
    /// private — an empty string (`visibility_modifier`).
    pub visibility_kind: &'static str,
    /// The child kind wrapping a function's modifier keywords
    /// (`function_modifiers`); an `async_kind` child of it marks an async fn.
    pub function_modifiers_kind: &'static str,
    /// The modifier token marking a function async (`async`).
    pub async_kind: &'static str,
    /// Field naming a `use` declaration's import tree (`argument`).
    pub argument_field: &'static str,
    /// Field naming the path of a `scoped_use_list_kind` / `use_as_clause_kind`
    /// (`path`).
    pub path_field: &'static str,
    /// Field naming the alias of a `use_as_clause_kind` (`alias`).
    pub alias_field: &'static str,
    /// Field naming the brace list of a `scoped_use_list_kind` (`list`).
    pub list_field: &'static str,
    /// Field naming the implemented trait on an `impl_kinds` node (`trait`);
    /// absent for an inherent impl.
    pub trait_field: &'static str,
    /// Field naming a call's argument list (`arguments`), scanned for
    /// `fn_value_arg_kinds` children.
    pub arguments_field: &'static str,
    /// Field naming a macro invocation's macro path (`macro`).
    pub macro_field: &'static str,
    /// Field naming a call expression's callee (`function`).
    pub callee_field: &'static str,
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
    /// Type-construction expression kinds — a syntactic construction of a named
    /// type that is NOT a call the call walker captures (Go `composite_literal`
    /// `OrderConfig{..}`; Rust `struct_expression` `OrderConfig{..}`; TS
    /// `new_expression` `new OrderConfig()`). When non-empty, the def walkers
    /// scan a function/method body for these nodes and record each constructed
    /// type's name (read from `construction_type_field`) in the enclosing
    /// callable's `constructed_types` property; the resolver's Uses phase binds
    /// each to its type node (`Uses_<caller>_<Type>`). Empty for every language
    /// that has not adopted the feature — an empty slice adds no property and no
    /// edge, so the untouched languages' parity is unchanged (issue #92, OCP:
    /// adoption is a data row, not new walker code). source: tree-sitter-go
    /// 0.25.0 / tree-sitter-rust 0.23.3 / tree-sitter-typescript 0.23.2
    /// node-types.json (validated by the spec guard).
    pub type_construction_kinds: &'static [&'static str],

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
    /// Field on a function/method node carrying its return-type annotation
    /// (Go `result`; Rust `return_type`; TS `return_type`). `Some` makes the def
    /// walkers record the return-type text in the callable's `return_type`
    /// property, which the resolver's Uses phase resolves to a `Uses_<caller>_
    /// <Type>` edge. `None` for every language that has not adopted the feature —
    /// no property, no edge, parity unchanged (issue #92). The text is stored
    /// verbatim minus a leading `:`/`->` (TS annotations, `: T`); the resolver's
    /// `extract_type_identifiers` strips generics/refs and keeps the nominal
    /// names. source: the grammar's node-types.json (validated by the guard).
    pub return_type_field: Option<&'static str>,
    /// Field on a `type_construction_kinds` node naming the constructed type
    /// (Go `composite_literal` `type`; Rust `struct_expression` `name`; TS
    /// `new_expression` `constructor`). `Some` iff `type_construction_kinds` is
    /// non-empty; the two are read together. `None` (and an empty
    /// `type_construction_kinds`) for every non-adopting language. source: the
    /// grammar's node-types.json (validated by the guard).
    pub construction_type_field: Option<&'static str>,

    // --- grammar factory, embedded rules, behavioral escape hatch ---
    /// Grammar factory (the Rust tree-sitter crate's `LANGUAGE`).
    pub ts_language: fn() -> TsLanguage,
    /// Embedded re-parse rules; empty for all core languages (ADR-0055 §3).
    pub embedded: &'static [EmbeddedSpec],
    /// Behavioral predicates and QN/entry shaping (ADR-0055 §4).
    pub conventions: &'static dyn LanguageConventions,
    /// When `Some`, this language is a flat C-family grammar: `walk_defs`
    /// delegates the whole file to the `clike` walker (consuming this sub-table)
    /// instead of the class-model arms. `None` for the class-model languages
    /// (Go/Python/Java/Kotlin/Swift), which leave the C-family walker untouched.
    pub c_family: Option<&'static CFamilySpec>,
    /// When `Some`, this language is a hybrid C-family class-model grammar (C++):
    /// `walk_defs` delegates the whole file to the `cpp` walker
    /// (consuming this sub-table) instead of the flat `clike` walker or the
    /// class-model arms. `None` for every other language. At most one of
    /// `c_family` / `cpp_family` / `objc_family` is `Some` on a given row.
    pub cpp_family: Option<&'static CppFamilySpec>,
    /// When `Some`, this language is Objective-C: `walk_defs` delegates the whole
    /// file to the `objc` walker (consuming this sub-table) instead of any other
    /// lane. `None` for every other language. At most one of `c_family` /
    /// `cpp_family` / `objc_family` / `ts_family` / `rust_family` is `Some` on a
    /// given row.
    pub objc_family: Option<&'static ObjcFamilySpec>,
    /// When `Some`, this language is TypeScript/TSX: `walk_defs` delegates the
    /// whole file to the `typescript` walker (consuming this sub-table) instead
    /// of any other lane. `None` for every other language.
    pub ts_family: Option<&'static TsFamilySpec>,
    /// A grammar factory that selects the tree-sitter `Language` by FILE PATH,
    /// for languages whose grammar depends on the file extension rather than a
    /// single fixed grammar. `Some` only for TypeScript, which ships two
    /// grammars (`typescript` and `tsx`): a `.tsx`/`.jsx`/`.js`/`.mjs`/`.cjs`
    /// file must parse with `tsx` (JSX is only in that grammar), a `.ts` file
    /// with `typescript`. When `Some`, `parse_with_spec` calls this with the
    /// file path instead of the fixed `ts_language`; `None` for every other
    /// language, which uses `ts_language`.
    pub ts_language_by_ext: Option<fn(&str) -> TsLanguage>,
    /// When `Some`, this language is Rust: `walk_defs` delegates the whole file
    /// to the `rust` walker (consuming this sub-table) instead of the flat
    /// `clike` walker, the hybrid `cpp` walker, or the class-model arms. `None`
    /// for every other language. At most one of `c_family` / `cpp_family` /
    /// `objc_family` / `ts_family` / `rust_family` is `Some` on a given row.
    pub rust_family: Option<&'static RustFamilySpec>,
}
