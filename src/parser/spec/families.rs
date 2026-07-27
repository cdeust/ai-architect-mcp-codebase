// parser::spec::families — the per-family structural sub-tables (ADR-0055).
//
// A grammar family whose shape the class-model `walk_defs` cannot express gets a
// dedicated walker driven by one of these sub-tables, and a `LangSpec` row
// selects it by carrying `c_family` / `cpp_family` / `objc_family` /
// `ts_family`. Each struct is pure DATA — node kinds and field names, every
// string traceable to that grammar's `node-types.json` and validated by the spec
// guard; the behavior that accompanies it lives in the row's
// `LanguageConventions` (ADR-0055 §4).
//
// Split out of `lang_spec.rs` along that concern boundary (the schema of a row
// vs. the schema of a family) to keep both files inside the §4.1 500-line cap:
// with four families inline, `lang_spec.rs` reached 547 lines. Distinct from the
// sibling `c_family` module, which holds shared BEHAVIOR (the C-family
// conventions helpers), not schema.

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

/// The node kinds an **ECMAScript-family** grammar (TypeScript/TSX, and any
/// future JS-family dialect) uses.
///
/// TypeScript fits neither the class-model `walk_defs` nor either C-family
/// walker, for four structural reasons its hand-written walker
/// (`parser::typescript::extract`) encoded:
///
/// 1. **Export is a wrapper, not a modifier.** `export function f(){}` puts the
///    declaration inside an `export_statement`; the wrapper — not the
///    declaration — carries the visibility. Worse, the two paths differ: a
///    declaration also counts as exported when its previous sibling is the
///    `export` token, but a `variable_declarator` NEVER consults that sibling and
///    relies solely on the inherited flag. `walk_defs` has no inherited-flag
///    concept, and `node_visibility(source, node, name)` cannot see a wrapper.
/// 2. **A `const` binding is a `Function` or a `Constant` depending on its
///    value.** `const f = () => {}` is a `Function` (whose *declarator* supplies
///    the line span and whose *arrow* supplies `is_async` and the call-scan
///    body); `const N = 1` is a `Constant`; `let x = 1` is neither. The generic
///    `walk_value_decl` emits only `Constant`s, and its stack DFS would descend
///    into nested declarators the hand-written walker never visited.
/// 3. **Class and interface bodies emit BOTH methods and fields**, from four
///    distinct member kinds (`method_definition`/`public_field_definition` in a
///    class, `method_signature`/`property_signature` in an interface), with
///    visibility read from an `accessibility_modifier` child. `walk_defs` has no
///    member-`Field`/`HasField` arm outside the Go type-spec path.
/// 4. **A def QN is NOT deduplicated.** A getter/setter pair (`get label()` /
///    `set label()`) legitimately produces two nodes sharing one QN, which
///    `emit_def`'s unconditional `ctx.dedup` would rewrite to `qn@line`.
///
/// So TypeScript gets a dedicated `walkers/ts` walker driven by this sub-table
/// — the #109 (`clike` for C) and #125 (`cpp` for C++) precedent — leaving
/// `walk_defs`/`clike`/`cpp` and the seven languages riding them untouched.
/// Calls and imports still route through the SHARED generic walkers
/// (`calls::walk_calls`, `imports::walk_imports`) via the conventions.
///
/// Every string traces to tree-sitter-typescript 0.23.2's `node-types.json` and
/// is validated by the spec guard against BOTH the `typescript` and `tsx`
/// dialects.
pub(crate) struct TsFamilySpec {
    /// Function declaration kinds → `Function` + `Defines` (TS
    /// `function_declaration`, `generator_function_declaration`).
    pub func_decl_kinds: &'static [&'static str],
    /// Class declaration kinds → `Struct` + `Defines`, heritage edges, and a
    /// class-body walk (TS `class_declaration`, `abstract_class_declaration`).
    pub class_decl_kinds: &'static [&'static str],
    /// Interface declaration kinds → `Trait` + `Defines`, `extends` edges, and
    /// an interface-body walk (TS `interface_declaration`).
    pub interface_decl_kinds: &'static [&'static str],
    /// Enum declaration kinds → `Enum` + `Defines` + members (TS
    /// `enum_declaration`).
    pub enum_decl_kinds: &'static [&'static str],
    /// Type-alias kinds → `TypeAlias` (`target_type` = the `value_field` text) +
    /// `Defines` (TS `type_alias_declaration`).
    pub type_alias_kinds: &'static [&'static str],
    /// Value-declaration statement kinds whose `declarator_kinds` children each
    /// become a `Function` (arrow value) or a `Constant` (`const` statement, per
    /// the conventions' `is_const_decl`) — TS `lexical_declaration`
    /// (`const`/`let`) and `variable_declaration` (`var`).
    pub value_decl_kinds: &'static [&'static str],
    /// The export-wrapper kinds whose declaration children are walked as
    /// EXPORTED (TS `export_statement`) — the ONLY export signal, since the
    /// walker's second one (an `export` previous sibling) proved unreachable and
    /// was removed (see `walkers::ts::decl_visibility`). Import statements are
    /// deliberately not dispatched inside the wrapper — the hand-written walker
    /// did not either.
    pub export_stmt_kinds: &'static [&'static str],
    /// The class-body kinds a class's `body_field` must have for its members to
    /// be walked (TS `class_body`).
    pub class_body_kinds: &'static [&'static str],
    /// Class-member method kinds → `Method` (`is_async`, `receiver_type`) +
    /// `HasMethod`, body scanned for calls (TS `method_definition` — which also
    /// covers `constructor`, `get`/`set` accessors, and `static` methods).
    pub method_kinds: &'static [&'static str],
    /// Class-member field kinds → `Field` (`type_annotation`) + `HasField`
    /// (TS `public_field_definition`).
    pub field_kinds: &'static [&'static str],
    /// The interface-body kinds an interface's `body_field` must have for its
    /// members to be walked (TS `interface_body`, `object_type`).
    pub interface_body_kinds: &'static [&'static str],
    /// Interface method-requirement kinds → `Method` (`is_async=false`,
    /// `receiver_type`) + `HasMethod`, no body ⇒ no calls (TS
    /// `method_signature`).
    pub method_signature_kinds: &'static [&'static str],
    /// Interface property kinds → `Field` (`type_annotation`) + `HasField`
    /// (TS `property_signature`).
    pub property_signature_kinds: &'static [&'static str],
    /// The enum-body kinds an enum's `body_field` must have for its members to
    /// be walked (TS `enum_body`).
    pub enum_body_kinds: &'static [&'static str],
    /// Enum members carrying an explicit value, named by `name_field` →
    /// `Variant` + `HasVariant` (TS `enum_assignment`: `Red = "RED"`).
    pub enum_member_kinds: &'static [&'static str],
    /// Bare enum members, named by their OWN text → `Variant` + `HasVariant`
    /// (TS `property_identifier`: `enum Bare { A, B }`). Separate from
    /// `enum_member_kinds` because the name comes from a different place.
    pub enum_bare_member_kinds: &'static [&'static str],
    /// The heritage container kinds, a direct child of a class node, holding the
    /// extends/implements clauses (TS `class_heritage`).
    pub class_heritage_kinds: &'static [&'static str],
    /// Clause kinds inside `class_heritage_kinds` whose named types → `Extends`
    /// (TS `extends_clause`).
    pub extends_clause_kinds: &'static [&'static str],
    /// Clause kinds inside `class_heritage_kinds` whose named types →
    /// `Implements` (TS `implements_clause`).
    pub implements_clause_kinds: &'static [&'static str],
    /// Clause kinds that are a DIRECT child of an interface node and whose named
    /// types → `Extends` (TS `extends_type_clause`).
    pub interface_extends_kinds: &'static [&'static str],
    /// Leaf kinds inside a heritage clause naming a supertype verbatim (TS
    /// `identifier`, `type_identifier`).
    pub heritage_name_kinds: &'static [&'static str],
    /// Generic-application kinds inside a CLASS heritage clause, whose
    /// `name_field` names the supertype (`extends Container<T>` → `Container`).
    /// Deliberately NOT consulted for `interface_extends_kinds`: the
    /// hand-written walker dropped `interface I extends Comparable<string>`, and
    /// parity preserves that asymmetry.
    pub generic_type_kinds: &'static [&'static str],
    /// Declarator kinds inside a `value_decl_kinds` statement, each binding one
    /// name via `name_field` (TS `variable_declarator`).
    pub declarator_kinds: &'static [&'static str],
    /// Value kinds that make a declarator a `Function` rather than a `Constant`
    /// (TS `arrow_function`).
    pub arrow_func_kinds: &'static [&'static str],
    /// Member visibility keyword kinds: the first such direct child's text IS
    /// the member's visibility (TS `accessibility_modifier` → `public` /
    /// `private` / `protected`); absent ⇒ empty visibility.
    pub visibility_modifier_kinds: &'static [&'static str],
    /// The import-clause kinds inside an import statement (TS `import_clause`).
    /// A statement with none is a side-effect import (`import 'polyfill'`).
    pub import_clause_kinds: &'static [&'static str],
    /// Named-import container kinds inside a clause (TS `named_imports`).
    pub named_imports_kinds: &'static [&'static str],
    /// Namespace-import kinds inside a clause (TS `namespace_import`:
    /// `import * as utils from '…'`), aliased by their identifier child.
    pub namespace_import_kinds: &'static [&'static str],
    /// One-name kinds inside a named-import container, carrying `name_field` and
    /// an optional `alias_field` (TS `import_specifier`).
    pub import_specifier_kinds: &'static [&'static str],
    /// Default-import kinds directly under a clause (TS `identifier`:
    /// `import Foo from 'bar'` → path `bar::default`).
    pub default_import_kinds: &'static [&'static str],
    /// The field holding a declarator's initializer and a type alias's target
    /// (TS `value`).
    pub value_field: &'static str,
    /// The field holding an import specifier's local alias (TS `alias`).
    pub alias_field: &'static str,
    /// The field holding an import statement's module string (TS `source`).
    pub import_source_field: &'static str,
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
