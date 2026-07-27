// parser::spec::families — the per-family structural sub-tables consumed by
// the dedicated walkers (C-family `clike`, C++ `cpp`, Objective-C `objc`,
// TypeScript `typescript`). Split from `lang_spec.rs` along the §4.1 concern
// boundary (the schema for the class-model `LangSpec` vs. the schemas for the
// dedicated-lane grammars) to keep both files under the 500-line cap. Pure
// move: every struct is re-exported from `lang_spec`, so no consumer import
// changes. Every node-kind / field string is validated by the spec guard.

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

/// The node kinds a **flat C-family** grammar (C, and later C++/ObjC) uses.
///
/// C-family languages do not fit the class-body-recursion model the generic
/// `walk_defs` was built around (Python/Java/Kotlin/Swift): they are flat
/// (structs carry *fields*, not methods), name their declarations through
/// wrapped declarators (`int (*handler)(int)` → the name is a `field_identifier`
/// buried under a `function_declarator`), model enum members and typedefs as
/// `Constant`s, filter function *declarations* (a prototype) apart from
/// variable declarations that share the `declaration` node kind, and recurse
/// transparently through preprocessor wrappers (`#ifdef … #endif`). When a
/// `LangSpec` carries `c_family: Some(_)`, `walk_defs` delegates the whole file
/// to the flat `clike` walker instead of the class-model arms.
///
/// This sub-table is the *shared* structural abstraction for the C family: the
/// dedup win ADR-0055 promises (`extract_function`×8, `extract_struct`×7) is
/// realized as C, C++, and ObjC migrate onto this one walker with three data
/// rows. Every string traces to tree-sitter-c's `node-types.json` and is
/// validated by the spec guard.
pub(crate) struct CFamilySpec {
    /// Struct/union declaration kinds → `Struct` + `Defines`, recursing into the
    /// `body_field` for `field_decl_kinds` (C `struct_specifier`/`union_specifier`).
    pub struct_like_kinds: &'static [&'static str],
    /// Enum declaration kinds → `Enum` + `Defines`, whose `enum_member_kinds`
    /// body children become `Constant`s (C `enum_specifier`).
    pub enum_like_kinds: &'static [&'static str],
    /// Enum-member kinds inside an enum body → `Constant` (`enum_entry=true`) +
    /// `Defines` under the enum's scope (C `enumerator`).
    pub enum_member_kinds: &'static [&'static str],
    /// Typedef kinds → `Constant` (`typedef=true`) + `Defines` (C `type_definition`).
    pub typedef_kinds: &'static [&'static str],
    /// Function-definition kinds → `Function` + `Defines`, scanning the body for
    /// calls (C `function_definition`).
    pub func_def_kinds: &'static [&'static str],
    /// Declaration kinds that MAY be a function prototype → `Function`
    /// (`is_prototype=true`) + `Defines`, only when a function declarator is
    /// present (`is_c_function_prototype`); a plain `int x;` variable declaration
    /// shares this kind and is skipped (C `declaration`).
    pub func_decl_kinds: &'static [&'static str],
    /// Member-declaration kinds inside a struct/union body → `Field` + `HasField`,
    /// one per declared name (C `field_declaration`; `int a, b;` is one node with
    /// two declarators).
    pub field_decl_kinds: &'static [&'static str],
    /// The declarator kind that marks a `func_decl_kinds` node as a function
    /// prototype (C `function_declarator`).
    pub func_declarator_kind: &'static str,
    /// The declarator wrapper that may itself hold a `func_declarator_kind`
    /// (`int f(void) = …`, rare — C `init_declarator`).
    pub init_declarator_kind: &'static str,
    /// The field naming a declaration's declarator, walked for field names and
    /// function names (C `declarator`).
    pub declarator_field: &'static str,
    /// The field holding a function declarator's parameter list (C
    /// `parameters`). A name search must SKIP this subtree: the parameters of
    /// `int add(int a, int b)` contain identifiers too, and descending into
    /// them is what made the function resolve to its last parameter (#106).
    pub parameters_field: &'static str,
    /// Leaf identifier kinds a name search unwraps to (C `identifier`,
    /// `type_identifier`) — the function/typedef/enum-member name is the first
    /// such leaf in a right-to-left DFS of the declarator.
    pub identifier_kinds: &'static [&'static str],
    /// Leaf kind naming a struct field, unwrapped from pointer/array/function
    /// declarators (C `field_identifier`).
    pub field_identifier_kind: &'static str,
    /// Object-like macro kinds → `Constant` (`macro=true`) + `Defines`, named by
    /// `name_field` (C `preproc_def`: `#define MAX 10`).
    pub macro_object_kinds: &'static [&'static str],
    /// Function-like macro kinds → `Function` (`macro=true`) + `Defines`, named
    /// by `name_field` (C `preproc_function_def`: `#define SQUARE(x) ((x)*(x))`).
    /// Separate from `macro_object_kinds` because the graph label differs —
    /// a function-like macro is callable, an object-like one is a value.
    pub macro_function_kinds: &'static [&'static str],
}

/// The node kinds a **hybrid C-family class-model** grammar (C++, and later
/// ObjC) uses.
///
/// C++ is neither the flat C family (`CFamilySpec`, whose structs carry
/// *fields* and whose enums emit members) nor the pure class-model
/// (`walk_defs`, whose classes recurse with `class_inheritance`/dedup). Its
/// hand-written walker (`parser::cpp::extract`) is a single class-recursive DFS
/// with semantics that match neither: a namespace is a `Struct` whose body
/// recurses as a NON-class scope (inner functions stay `Function`s, not
/// methods); a class/struct/union is a `Struct` whose body recurses as a class
/// scope; a member `field_declaration` becomes a `Constant` (a data member) OR
/// a `Method` (`is_prototype`, when it carries a function declarator), NOT a
/// `Field`/`HasField`; an enum emits NO members; a typedef is a `Constant`; a
/// `using`/`#include` is an `Import`; and a single per-file `seq` counter keys
/// functions, prototypes, AND call sites in one DFS order. That last property
/// makes exact parity possible only by reproducing the DFS itself, so C++ gets
/// a dedicated `walkers/cpp` walker driven by this sub-table (the #109 precedent
/// that added `walkers/clike` for C), leaving `walk_defs`/`clike` — and the six
/// languages that ride them — untouched.
///
/// When a `LangSpec` carries `cpp_family: Some(_)`, `walk_defs` delegates the
/// whole file to the `cpp` walker. Every string traces to tree-sitter-cpp's
/// `node-types.json` and is validated by the spec guard. Import/call/QN
/// behavior lives in the companion `CppConventions` (ADR-0055 §4).
pub(crate) struct CppFamilySpec {
    /// Namespace declaration kinds → `Struct` (`is_namespace=true`) + `Defines`,
    /// recursing into `body_field` as a NON-class scope (C++
    /// `namespace_definition`). An anonymous namespace emits no node but still
    /// recurses its body under the unchanged scope.
    pub namespace_kinds: &'static [&'static str],
    /// Class declaration kinds → `Struct` (`is_class=true`) + `Defines`, base
    /// clause → `Extends`, body recursed as a CLASS scope (C++ `class_specifier`).
    pub class_kinds: &'static [&'static str],
    /// Struct/union declaration kinds → `Struct` (no `is_class`) + `Defines`,
    /// same base-clause + class-scope-recursion as `class_kinds` (C++
    /// `struct_specifier`/`union_specifier`).
    pub struct_kinds: &'static [&'static str],
    /// Enum declaration kinds → `Enum` + `Defines`, with NO body recursion —
    /// the hand-written walker never emitted enum members (C++ `enum_specifier`,
    /// which also covers `enum class`). Preserved for parity.
    pub enum_kinds: &'static [&'static str],
    /// Wrapper kinds walked transparently (same scope + enclosing type, no node
    /// emitted) because they wrap a class/function (C++ `template_declaration`).
    pub template_kinds: &'static [&'static str],
    /// Function-definition kinds → `Function` + `Defines` (at file/namespace
    /// scope) or `Method` + `HasMethod` (inside a class body, receiver-scoped),
    /// scanning `body_field` for calls (C++ `function_definition`).
    pub func_def_kinds: &'static [&'static str],
    /// Member-declaration kinds inside a class/struct body: a `Method`
    /// (`is_prototype`) when a function declarator is present, else a data-member
    /// `Constant` (C++ `field_declaration`). Ignored outside a class body.
    pub field_decl_kinds: &'static [&'static str],
    /// Typedef kinds → `Constant` (`typedef=true`) + `Defines` (C++
    /// `type_definition`).
    pub typedef_kinds: &'static [&'static str],
    /// The declarator kind that marks a `field_decl_kinds` member as a method
    /// prototype rather than a data member (C++ `function_declarator`).
    pub func_declarator_kind: &'static str,
    /// The field naming a `func_def_kinds` node's declarator, searched for the
    /// function/method name (C++ `declarator`).
    pub declarator_field: &'static str,
    /// The base-class clause kind, a direct child of a class/struct node whose
    /// `base_type_kinds` children each name a superclass (C++ `base_class_clause`).
    pub base_clause_kind: &'static str,
    /// Child kinds of a `base_clause_kind` naming a base type → `Extends`
    /// (C++ `type_identifier`/`qualified_identifier`/`template_type`; access
    /// specifiers and virtual/attribute tokens are skipped).
    pub base_type_kinds: &'static [&'static str],
    /// Leaf identifier kinds a name search unwraps to, in a right-to-left DFS
    /// (C++ `identifier`/`type_identifier`/`field_identifier`). The DFS order is
    /// load-bearing: for a declarator with named parameters it lands on the LAST
    /// parameter name, a pre-existing naming defect the migration preserves.
    pub identifier_kinds: &'static [&'static str],
}

/// The node kinds an **Objective-C hybrid** grammar uses.
///
/// Objective-C fits none of the existing lanes. It is a C superset (plain C
/// structs/unions/enums/typedefs and functions appear in a `.m`/`.h` file) AND
/// carries an ObjC object model whose shapes match neither the flat C family
/// (`CFamilySpec`) nor the C++ hybrid (`CppFamilySpec`): a class
/// (`@interface`/`@implementation`) is a `Struct` keyed by name (so an
/// interface, its implementation, and a category all share one QN), a category
/// is that same node carrying a `category` field, a `@protocol` is a `Trait`
/// with NO member extraction, a method's name is a reconstructed SELECTOR
/// (`doWith:andThen:`) not a plain identifier, and a message send
/// (`[obj do:x]`) is a `Calls` edge whose callee is the reconstructed selector.
/// A single per-file `seq` counter keys methods, functions, AND call sites in
/// one DFS order (as in C++), so exact parity requires reproducing that DFS —
/// hence a dedicated `walkers/objc` walker driven by this sub-table (the #109
/// `clike` / #125 `cpp` precedent), leaving the other lanes untouched.
///
/// Its C-side name resolution deliberately differs from `CFamilySpec`'s: the
/// hand-written ObjC walker named C structs/enums by the `name` field then the
/// first identifier leaf (NOT the parameter-skipping declarator chain), named
/// functions by the declarator's `declarator` field, named typedefs by the LAST
/// `type_identifier` under the declarator, and did NOT recurse inline struct
/// definitions inside a typedef. Those differences are preserved for parity, so
/// ObjC gets its own sub-table and walker rather than reusing `clike`. When a
/// `LangSpec` carries `objc_family: Some(_)`, `walk_defs` delegates the whole
/// file to the `objc` walker. Every string traces to tree-sitter-objc's
/// `node-types.json`, validated by the spec guard. Import/call behavior lives in
/// the companion `ObjcConventions` (ADR-0055 §4).
pub(crate) struct ObjcFamilySpec {
    /// Class declaration kinds → `Struct` (keyed by name), superclass
    /// (`superclass_field`) → `Extends`, `category_field` → `is_category`/
    /// `category` props, then walked for `method_kinds` members
    /// (`class_interface`/`class_implementation`).
    pub class_kinds: &'static [&'static str],
    /// Protocol declaration kinds → `Trait` + `Defines`, with NO member
    /// extraction (`protocol_declaration`). Preserved for parity.
    pub protocol_kinds: &'static [&'static str],
    /// Method declaration/definition kinds → `Method` + `HasMethod`, keyed by a
    /// reconstructed selector, `receiver_type` = the enclosing class QN
    /// (`method_declaration`/`method_definition`).
    pub method_kinds: &'static [&'static str],
    /// Free-function definition kinds → `Function` + `Defines` at file scope,
    /// scanning the body for calls (`function_definition`).
    pub func_def_kinds: &'static [&'static str],
    /// C struct/union kinds → `Struct` + `Defines`, whose `field_decl_kinds`
    /// body members become `Field` + `HasField`
    /// (`struct_specifier`/`union_specifier`).
    pub struct_kinds: &'static [&'static str],
    /// C enum kinds → `Enum` + `Defines`, whose `enum_member_kinds` become
    /// `Constant` (`enum_entry=true`) (`enum_specifier`).
    pub enum_kinds: &'static [&'static str],
    /// Enum-member kinds inside an enum body → `Constant` (`enumerator`).
    pub enum_member_kinds: &'static [&'static str],
    /// Typedef kinds → `Constant` (`typedef=true`) (`type_definition`).
    pub typedef_kinds: &'static [&'static str],
    /// Member-declaration kinds inside a C struct/union body → `Field` +
    /// `HasField`, one per declarator (`field_declaration`).
    pub field_decl_kinds: &'static [&'static str],
    /// The field on a class node naming a category (`category`); its presence
    /// marks the class as a category.
    pub category_field: &'static str,
    /// The field on a class node naming its superclass → `Extends`
    /// (`superclass`).
    pub superclass_field: &'static str,
    /// The field naming a function definition's declarator, read for the
    /// function name (`declarator`).
    pub declarator_field: &'static str,
    /// Body kinds a function/method scans for calls when the `body_field` is
    /// absent (`compound_statement`).
    pub func_body_kinds: &'static [&'static str],
    /// Leaf kind naming a C struct field, unwrapped from pointer/array
    /// declarators in a DFS (`field_identifier`).
    pub field_identifier_kind: &'static str,
    /// Leaf identifier kinds a class/protocol name search accepts, in source
    /// order (`identifier`/`type_identifier`).
    pub identifier_kinds: &'static [&'static str],
    /// The bare identifier kind a C struct/enum name fallback and a selector
    /// keyword read — the FIRST direct child of exactly this kind
    /// (`identifier`). Narrower than `identifier_kinds` on purpose: an anonymous
    /// struct/enum (no `name` field and no `identifier` child) must resolve to
    /// empty and be skipped, so this must NOT also match `type_identifier`.
    pub plain_identifier_kind: &'static str,
    /// Leaf kind a typedef name search unwraps to, taking the LAST such leaf
    /// under the declarator (`type_identifier`).
    pub typedef_name_kind: &'static str,
}

/// The node kinds a **TypeScript / TSX** grammar uses (ADR-0055 phase 9).
///
/// TypeScript is class-model-shaped (classes with methods, interfaces, enums,
/// `extends`/`implements`) yet fits none of the existing lanes cleanly — the
/// evidence from the hand-written walker (`parser::typescript::extract`) shows
/// six structural divergences the generic class-model arms (`walk_defs`) cannot
/// express without perturbing the six languages that ride them:
///   1. Class members are `Field` + `HasField` (a `public_field_definition`),
///      whereas the generic class-body model emits class members as `Constant`
///      (Java's `variable_field_kinds`). Interface `property_signature`s are
///      likewise `Field`s.
///   2. A `variable_declarator` is polymorphic: an `arrow_function` value → a
///      call-scanning `Function`; a `const` non-arrow → a `Constant`; a `let`
///      non-arrow → nothing. The generic value path emits only `Constant`s and
///      never scans a body.
///   3. A call site emits TWO refs — `Defines`(caller → call-site node) AND
///      `Calls`(caller → callee tail) — whereas the generic `walk_calls` emits
///      exactly one ref per call.
///   4. A getter and setter of the same name are TWO `Method` nodes on the
///      IDENTICAL QN (no dedup); the generic `emit_def` force-dedups colliding
///      QNs (`qn@line`), which would diverge.
///   5. A `type_alias_declaration` → `TypeAlias` with a `target_type` property;
///      the generic model surfaces `TypeAlias` only via Go's `type_spec` path.
///   6. A bare enum member (`property_identifier`, no `= value`) → `Variant`;
///      the generic `emit_variant` reads `name_field` children a bare member
///      lacks.
///
/// Reproducing these at exact parity inside `walk_defs` would add six TS-only
/// arms to the generic walker (risking the eight parity corpora that ride it)
/// or bypass its dedup invariant. So TypeScript gets a dedicated
/// `walkers/typescript` walker driven by this sub-table, exactly as C++ (#125)
/// and Objective-C (#138) did for grammars that "fit neither the flat `clike`
/// walker nor the class-model arms". When a `LangSpec` carries
/// `ts_family: Some(_)`, `walk_defs` delegates the whole file to that walker.
///
/// Import/call/QN/visibility behavior lives in the companion `TsConventions`
/// (ADR-0055 §4). Every string traces to tree-sitter-typescript 0.23.2's
/// `node-types.json` and is validated by the spec guard.
pub(crate) struct TsFamilySpec {
    /// Type-alias declaration kinds → `TypeAlias` + `Defines`, `value_field`
    /// text → `target_type` prop (`type_alias_declaration`).
    pub type_alias_kinds: &'static [&'static str],
    /// Statement kinds wrapping an exported declaration: recursed transparently
    /// with the export flag set, so the inner def/class/const is `pub`
    /// (`export_statement`).
    pub export_kinds: &'static [&'static str],
    /// Method-definition kinds inside a class body → `Method` + `HasMethod`
    /// (`method_definition`; getters/setters share this kind and this QN).
    pub method_def_kinds: &'static [&'static str],
    /// Public-field-definition kinds inside a class body → `Field` + `HasField`
    /// (`public_field_definition`).
    pub field_def_kinds: &'static [&'static str],
    /// Method-signature kinds inside an interface body → `Method` + `HasMethod`
    /// (`method_signature`; `is_async=false`).
    pub method_sig_kinds: &'static [&'static str],
    /// Property-signature kinds inside an interface body → `Field` + `HasField`
    /// (`property_signature`).
    pub property_sig_kinds: &'static [&'static str],
    /// Class-body container kinds recursed for members (`class_body`).
    pub class_body_kinds: &'static [&'static str],
    /// Interface-body container kinds recursed for members (`interface_body`,
    /// `object_type`).
    pub interface_body_kinds: &'static [&'static str],
    /// Enum-body container kinds recursed for members (`enum_body`).
    pub enum_body_kinds: &'static [&'static str],
    /// Enum-member kinds carrying a `= value` → `Variant` named by `name_field`
    /// (`enum_assignment`).
    pub enum_assignment_kinds: &'static [&'static str],
    /// Bare enum-member kinds (no value) → `Variant` named by the node's own
    /// text (`property_identifier`).
    pub enum_member_ident_kinds: &'static [&'static str],
    /// Class-heritage wrapper kinds holding extends/implements clauses
    /// (`class_heritage`).
    pub heritage_kinds: &'static [&'static str],
    /// Extends-clause kinds inside class heritage → `Extends` (`extends_clause`).
    pub extends_clause_kinds: &'static [&'static str],
    /// Implements-clause kinds inside class heritage → `Implements`
    /// (`implements_clause`).
    pub implements_clause_kinds: &'static [&'static str],
    /// Interface extends-clause kinds → `Extends` (`extends_type_clause`).
    pub interface_extends_kinds: &'static [&'static str],
    /// Leaf kinds naming a base type inside a heritage clause
    /// (`identifier`/`type_identifier`).
    pub heritage_name_kinds: &'static [&'static str],
    /// Generic-type kinds inside a heritage clause; the base name is the node's
    /// `name_field` child (`generic_type`: `extends Wrapper<T>` → `Wrapper`).
    pub generic_type_kinds: &'static [&'static str],
    /// Import-clause kinds inside an import statement (`import_clause`).
    pub import_clause_kinds: &'static [&'static str],
    /// Named-imports group kinds inside an import clause (`named_imports`).
    pub named_imports_kinds: &'static [&'static str],
    /// Namespace-import kinds inside an import clause → glob import
    /// (`namespace_import`: `import * as x`).
    pub namespace_import_kinds: &'static [&'static str],
    /// Import-specifier kinds inside a named-imports group, named by
    /// `name_field`, aliased by `alias_field` (`import_specifier`).
    pub import_specifier_kinds: &'static [&'static str],
    /// Default-import leaf kinds inside an import clause → `{path}::default`
    /// (`identifier`: `import Foo from 'bar'`).
    pub default_import_ident_kinds: &'static [&'static str],
    /// Variable-declarator kinds inside a value declaration (`variable_declarator`).
    pub declarator_kinds: &'static [&'static str],
    /// Arrow-function value kinds that make a declarator a `Function`
    /// (`arrow_function`).
    pub arrow_kinds: &'static [&'static str],
    /// Accessibility-modifier kinds giving a member its visibility text
    /// (`accessibility_modifier`: `public`/`private`/`protected`).
    pub accessibility_kinds: &'static [&'static str],
    /// Abstract-method-signature kinds inside a class body → `Method` +
    /// `HasMethod` (bodiless, so no calls scanned), like a concrete
    /// `method_definition` (`abstract_method_signature`; issue #141). Mirrors
    /// Swift's bodiless `protocol_function_declaration` and Java's `abstract`
    /// `method_declaration`, which both extract the requirement as a `Method`.
    pub abstract_method_sig_kinds: &'static [&'static str],
    /// Object-literal value kinds whose `method_definition` and arrow-property
    /// bodies are scanned for calls under the enclosing const's QN
    /// (`object`; issue #142).
    pub object_literal_kinds: &'static [&'static str],
    /// Property-pair kinds inside an object literal (`key: value`); a pair whose
    /// value is an `arrow_function` has its body scanned for calls (`pair`;
    /// issue #142).
    pub pair_kinds: &'static [&'static str],
    /// Field naming an import statement's module string (`source`).
    pub source_field: &'static str,
    /// Field naming a declarator's / type-alias's value (`value`).
    pub value_field: &'static str,
    /// Field naming an import specifier's alias (`alias`).
    pub alias_field: &'static str,
}
