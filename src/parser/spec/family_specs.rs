// parser::spec::family_specs — the C-family structural sub-tables (ADR-0055).
//
// Split out of `lang_spec` along the concern boundary the file grew across: that
// module describes the GENERIC extraction schema every language shares, while
// these four types describe the three C-family grammars specifically — the shared
// declarator-naming data plus one sub-table per lane (flat C, hybrid C++,
// Objective-C). Keeping them apart is also what holds both files under the
// 500-line cap (coding-standards §4.1).
//
// Every string in every `*_kinds` / `*_kind` / `*_field` below traces to that
// grammar's `node-types.json` and is validated by the spec guard
// (`guard::every_spec_node_kind_is_real_for_its_grammar`), which is what makes the
// §8 citation executable rather than decorative.

/// How a C-family grammar spells the name of a declaration — the data the ONE
/// shared `declarator::declarator_name` search consumes (issues #106 and #123).
///
/// Both C-family sub-tables carry this, so the "name comes from the declarator
/// chain, never from the parameter list" rule has a single implementation and
/// per-language DATA rather than a copy per walker. That was the whole point of
/// #106's fix: the skip is data (`parameters_field`), not a hardcoded branch, so
/// the next C-family grammar inherits the corrected behavior by adding a row.
///
/// Every string is validated against its grammar's `node-types.json` by the spec
/// guard (`guard::spec_node_kinds` / `guard::spec_field_names`).
pub(crate) struct DeclaratorNaming {
    /// Leaf identifier kinds a name search unwraps to (C `identifier`,
    /// `type_identifier`; C++ adds `field_identifier` for class members). The
    /// name is the identifier the declarator's own `declarator_field` chain
    /// binds.
    pub identifier_kinds: &'static [&'static str],
    /// Kinds that ARE the name, taken as their own source text rather than
    /// descended into. C++ `destructor_name` (`~Point`) and `operator_name`
    /// (`operator+`) both wrap an inner `identifier` whose text would be the
    /// WRONG name (`Point`, and nothing at all), so they terminate the search.
    /// Empty for C, which has neither construct.
    pub name_text_kinds: &'static [&'static str],
    /// The field naming a declaration's declarator, followed down to the
    /// identifier leaf (C/C++ `declarator`).
    pub declarator_field: &'static str,
    /// The field holding a function declarator's parameter list (C/C++
    /// `parameters`). A name search must SKIP this subtree: the parameters of
    /// `int add(int a, int b)` contain identifiers too, and descending into them
    /// is what made the function resolve to its last parameter (#106 for C, #123
    /// for C++).
    pub parameters_field: &'static str,
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
    /// How this grammar spells a declaration's name (shared with the C++
    /// sub-table so `declarator_name` has one implementation — #106/#123).
    pub naming: &'static DeclaratorNaming,
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
/// C++ is neither the flat C family (`CFamilySpec`, which knows nothing of
/// namespaces, classes, or methods) nor the pure class-model (`walk_defs`, whose
/// classes recurse with `class_inheritance`/dedup). Its walker is a single
/// class-recursive DFS with semantics that match neither: a namespace is a
/// `Struct` whose body recurses as a NON-class scope (inner functions stay
/// `Function`s, not methods); a class/struct/union is a `Struct` whose body
/// recurses as a class scope; a member is a `Method` (`is_prototype`, when it
/// carries a function declarator) or a `Field`/`HasField` data member; a
/// `using X = Y;` is a `TypeAlias` while a `using namespace`/`#include` is an
/// `Import`; and a single per-file `seq` counter keys functions, methods,
/// prototypes, AND call sites in one DFS order. That last property means the
/// emission ORDER is part of the contract, so C++ keeps a dedicated
/// `walkers/cpp` walker driven by this sub-table (the #109 precedent that added
/// `walkers/clike` for C), leaving `walk_defs`/`clike` — and the six languages
/// that ride them — untouched.
///
/// What C++ *does* share with the flat family is the C declarator grammar, and
/// that sharing is explicit: `naming` is the same `DeclaratorNaming` sub-table C
/// carries, consumed by the same `declarator::declarator_name` (#106/#123). The
/// duplication ADR-0055 warns about is avoided at the mechanism, not by forcing
/// two different structural models through one walker.
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
    /// Enum declaration kinds → `Enum` + `Defines`, recursing into `body_field`
    /// for `enum_member_kinds` (C++ `enum_specifier`, which also covers
    /// `enum class`).
    pub enum_kinds: &'static [&'static str],
    /// Enum-member kinds inside an enum body → `Constant` (`enum_entry=true`) +
    /// `Defines` under the enum's scope (C++ `enumerator`). The name is the
    /// member's `name_field`, so a valued member (`GREEN = 5`) resolves to
    /// `GREEN` and not to its value expression (issue #124 item 1).
    pub enum_member_kinds: &'static [&'static str],
    /// Wrapper kinds walked transparently (same scope + enclosing type, no node
    /// emitted) because they wrap a class/function (C++ `template_declaration`).
    pub template_kinds: &'static [&'static str],
    /// Function-definition kinds → `Function` + `Defines` (at file/namespace
    /// scope) or `Method` + `HasMethod` (inside a class body, receiver-scoped),
    /// scanning `body_field` for calls (C++ `function_definition`).
    pub func_def_kinds: &'static [&'static str],
    /// Member-declaration kinds inside a class/struct body: a `Method`
    /// (`is_prototype`) when a function declarator is present, else one `Field` +
    /// `HasField` per declared name (C++ `field_declaration`). Ignored outside a
    /// class body. A member declaring a nested TYPE carries the specifier in its
    /// `type_field` instead of a declarator (issue #124 item 4).
    pub field_decl_kinds: &'static [&'static str],
    /// Member-declaration kinds inside a class body that carry NO type — a
    /// constructor or destructor declaration (`Point(int,int);`, `~Point();`),
    /// which the grammar spells as a plain `declaration`, not a
    /// `field_declaration` (issue #124 item 2). Emitted as a prototype `Method`
    /// ONLY inside a class body and ONLY when a function declarator is present;
    /// the same kind at file/namespace scope keeps falling through to the
    /// transparent-recursion arm (where `struct S { int x; } v;` is reached).
    pub member_decl_kinds: &'static [&'static str],
    /// Typedef kinds → `Constant` (`typedef=true`) + `Defines` (C++
    /// `type_definition`).
    pub typedef_kinds: &'static [&'static str],
    /// Alias-declaration kinds → `TypeAlias` + `Defines` (C++
    /// `alias_declaration`: `using Distance = double;`). A DIFFERENT node kind
    /// from `using_declaration` (`using namespace std;`), which stays an
    /// `Import` via `import_node_kinds` (issue #124 item 3).
    pub alias_kinds: &'static [&'static str],
    /// The declarator kind that marks a `field_decl_kinds` member as a method
    /// prototype rather than a data member (C++ `function_declarator`).
    pub func_declarator_kind: &'static str,
    /// Declarator kinds that make the name they wrap a POINTER or REFERENCE rather
    /// than the thing itself (C++ `pointer_declarator`, `reference_declarator`).
    /// Load-bearing for issue #135: `void (*cb)(int);` and `void cb(int);` share
    /// the same outer `function_declarator`, and only a pointer declarator BETWEEN
    /// that function declarator and the name distinguishes a data member of
    /// function-pointer type from a method.
    pub pointer_declarator_kinds: &'static [&'static str],
    /// Declarator kinds that only GROUP, binding nothing themselves (C++
    /// `parenthesized_declarator`) — traversed transparently when reading what a
    /// declarator declares.
    pub grouping_declarator_kinds: &'static [&'static str],
    /// The declarator kind that names an owner-qualified definition
    /// (C++ `qualified_identifier`: `geometry::Circle::area`). Its
    /// `qualified_scope_field` segments are the owner path an out-of-body
    /// definition re-attaches to (issue #124 item 5).
    pub qualified_declarator_kind: &'static str,
    /// The field holding one qualifier segment of a `qualified_declarator_kind`
    /// (C++ `scope`); the remainder hangs off `name_field`, nesting once per
    /// `::`. Verified against node-types.json: `qualified_identifier` has
    /// exactly `scope` + `name`.
    pub qualified_scope_field: &'static str,
    /// The base-class clause kind, a direct child of a class/struct node whose
    /// `base_type_kinds` children each name a superclass (C++ `base_class_clause`).
    pub base_clause_kind: &'static str,
    /// Child kinds of a `base_clause_kind` naming a base type → `Extends`
    /// (C++ `type_identifier`/`qualified_identifier`/`template_type`; access
    /// specifiers and virtual/attribute tokens are skipped).
    pub base_type_kinds: &'static [&'static str],
    /// How this grammar spells a declaration's name — the SAME sub-table type the
    /// flat C family carries, so `declarator::declarator_name` (the #106 fix) is
    /// inherited as data rather than reimplemented for C++ (#123).
    pub naming: &'static DeclaratorNaming,
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
    /// Kinds that carry one argument of a keyword selector (ObjC
    /// `method_parameter`). A selector keyword takes an argument — and therefore a
    /// trailing `:` in the selector — exactly when the next DIRECT child of the
    /// method node is one of these, which is how the full selector `setX:y:` is
    /// reconstructed (issue #128).
    pub method_parameter_kinds: &'static [&'static str],
    /// Leaf kind a typedef name search unwraps to, taking the LAST such leaf
    /// under the declarator (`type_identifier`).
    pub typedef_name_kind: &'static str,
}
