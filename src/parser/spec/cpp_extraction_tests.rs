// parser::spec::cpp_extraction_tests — one targeted pin per acceptance clause of
// issues #123 (declarator names) and #124 (extraction-shape gaps).
//
// The corpus-wide exact-set assertion lives in `cpp_parity_tests`; it proves the
// WHOLE output is right but says nothing about WHICH clause each row satisfies.
// These tests are the per-clause ledger: each names the issue item it closes and
// asserts the minimum observable consequence, so a regression points at one
// clause instead of a 60-row set diff. Each one was RUN against the pre-fix
// walker and failed (the failures are quoted in the PR's verification section).
//
// Every node kind and field named here is verified against tree-sitter-cpp
// 0.23.4's src/node-types.json; the AST shapes were dumped from the grammar
// before the fix, not assumed.
// source: tree-sitter-cpp 0.23.4 src/node-types.json.

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language};

const PATH: &str = "a.cpp";

fn parse(src: &str) -> (Vec<ExtractedNode>, Vec<ExtractedRef>) {
    let r = parse_file(src, PATH, Language::Cpp).expect("cpp parse must not hard-fail");
    assert_eq!(r.parse_errors, 0, "fixture must parse clean:\n{src}");
    (r.nodes, r.refs)
}

/// The full 7-field record of every node with `name`, for an exact assertion
/// that also pins label/QN/lines/visibility/properties (not just presence).
fn records_named(nodes: &[ExtractedNode], name: &str) -> Vec<String> {
    let mut out: Vec<String> = nodes
        .iter()
        .filter(|n| n.name == name)
        .map(|n| {
            format!(
                "{}|{}|{}|{}|{}|{}|{:?}",
                n.label,
                n.name,
                n.qualified_name,
                n.start_line,
                n.end_line,
                n.visibility,
                n.properties
            )
        })
        .collect();
    out.sort();
    out
}

/// Every `(label, name)` pair emitted, sorted — the shape assertion used by the
/// negative tests (absence IS the behavior, §13.1 G4).
fn label_names(nodes: &[ExtractedNode]) -> Vec<String> {
    let mut out: Vec<String> = nodes
        .iter()
        .map(|n| format!("{}|{}", n.label, n.name))
        .collect();
    out.sort();
    out
}

fn refs_of(refs: &[ExtractedRef], kind: &str) -> Vec<String> {
    let mut out: Vec<String> = refs
        .iter()
        .filter(|r| r.kind == kind)
        .map(|r| format!("{} -> {}", r.from_qualified_name, r.to_qualified_name))
        .collect();
    out.sort();
    out
}

// ---- #123: names come from the declarator, never from a parameter ----------

/// #123 acceptance clause 2, verbatim: "`freeFunction`, `operator+`, `add`,
/// `get`, `magnitude`, `area`, `draw` resolve to their declared names."
#[test]
fn cpp_declared_names_never_resolve_to_a_parameter() {
    let src = r#"struct Point {
    int magnitude() const;
};
class Shape {
public:
    virtual double area() const;
    Shape operator+(const Shape& other);
};
class Multi {
    void draw();
};
template <typename T>
class Container {
public:
    void add(T item);
    T get(int i);
};
template <typename T>
T identity(T x) { return x; }
int freeFunction(int a, int b) { return a + b; }
"#;
    let (nodes, _refs) = parse(src);
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    for declared in [
        "magnitude",
        "area",
        "operator+",
        "draw",
        "add",
        "get",
        "identity",
        "freeFunction",
    ] {
        assert!(
            names.contains(&declared),
            "#123: declared name {declared:?} missing; got {names:?}"
        );
    }
    // The parameter names must NOT appear as declaration names — that IS the
    // defect (negative assertion, §13.1 G4).
    for param in ["other", "item", "i", "x", "b"] {
        assert!(
            !names.contains(&param),
            "#123: parameter name {param:?} was used as a declaration name; got {names:?}"
        );
    }
}

/// The declarator wrappers the field chain must traverse: a returned reference
/// (`reference_declarator`, which has NO fields — verified in node-types.json)
/// and a function returning a function pointer (`function_declarator` >
/// `parenthesized_declarator` > `pointer_declarator` > `function_declarator`).
/// Both put an identifier inside the parameter list too, so a name search that
/// descends into `parameters` lands on the wrong leaf.
#[test]
fn cpp_declarator_name_traverses_pointer_and_reference_wrappers() {
    let src = "int& refret(int q) { static int s; return s; }\n\
               int (*table(int n))(int) { return 0; }\n";
    let (nodes, _refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec!["Function|refret".to_string(), "Function|table".to_string()],
        "#123: wrapped declarators must resolve to the declared function name"
    );
}

// ---- #124.1: enum members surface under their enum -------------------------

#[test]
fn cpp_enum_members_are_emitted_under_their_enum() {
    let src = "enum Color { RED, GREEN = 5, BLUE };\nenum class Status { OK, FAIL };\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        records_named(&nodes, "RED"),
        vec!["Constant|RED|a.cpp::Color::RED|1|1|public|[(\"enum_entry\", \"true\")]".to_string()],
        "#124.1: plain-enum member must be a Constant scoped under its enum"
    );
    // A valued enumerator still resolves to its NAME, not to the value literal.
    assert_eq!(
        records_named(&nodes, "GREEN"),
        vec![
            "Constant|GREEN|a.cpp::Color::GREEN|1|1|public|[(\"enum_entry\", \"true\")]"
                .to_string()
        ],
        "#124.1: `GREEN = 5` must resolve to GREEN"
    );
    assert_eq!(
        records_named(&nodes, "OK"),
        vec!["Constant|OK|a.cpp::Status::OK|2|2|public|[(\"enum_entry\", \"true\")]".to_string()],
        "#124.1: `enum class` members must surface too (same enum_specifier kind)"
    );
    assert!(
        refs_of(&refs, "Defines").contains(&"a.cpp::Color -> a.cpp::Color::BLUE".to_string()),
        "#124.1: each member needs a Defines edge from its enum; got {:?}",
        refs_of(&refs, "Defines")
    );
}

// ---- #124.2: constructors and destructors surface as methods ---------------

#[test]
fn cpp_constructors_and_destructors_are_methods() {
    let src = "class Shape {\npublic:\n    Shape();\n    virtual ~Shape();\n};\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        records_named(&nodes, "Shape")
            .into_iter()
            .filter(|r| r.starts_with("Method|"))
            .collect::<Vec<_>>(),
        vec![
            "Method|Shape|a.cpp::Shape::Shape#1|3|3|public|[(\"is_prototype\", \"true\"), \
             (\"receiver_type\", \"a.cpp::Shape\")]"
                .to_string()
        ],
        "#124.2: a constructor declaration must surface as a prototype Method"
    );
    assert_eq!(
        records_named(&nodes, "~Shape"),
        vec![
            "Method|~Shape|a.cpp::Shape::~Shape#2|4|4|public|[(\"is_prototype\", \"true\"), \
             (\"receiver_type\", \"a.cpp::Shape\")]"
                .to_string()
        ],
        "#124.2: a destructor keeps its declared spelling `~Shape` (destructor_name text), \
         so it is distinguishable from the constructor"
    );
    assert_eq!(
        refs_of(&refs, "HasMethod"),
        vec![
            "a.cpp::Shape -> a.cpp::Shape::Shape#1".to_string(),
            "a.cpp::Shape -> a.cpp::Shape::~Shape#2".to_string(),
        ],
        "#124.2: both must be attached to the class"
    );
}

/// A ctor/dtor declaration is a `declaration` node, a kind that ALSO appears at
/// file and namespace scope (`int g;`, `struct S { int x; } v;`). Intercepting
/// it must stay confined to a class body, and must not consume the
/// transparent-recursion arm that reaches an inline type at file scope.
#[test]
fn cpp_declaration_outside_a_class_body_is_not_a_method() {
    let src = "int globalVar;\nstruct Wrapped { int x; } inst;\n";
    let (nodes, _refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec!["Field|x".to_string(), "Struct|Wrapped".to_string(),],
        "#124.2: a file-scope `declaration` must not become a Method, and the inline \
         struct it wraps must still be reached by the recursion arm"
    );
}

// ---- #124.3: `using X = Y;` surfaces as a type alias ------------------------

#[test]
fn cpp_alias_declaration_is_a_type_alias() {
    let src = "namespace geometry {\nusing Distance = double;\n}\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        records_named(&nodes, "Distance"),
        vec!["TypeAlias|Distance|a.cpp::geometry::Distance|2|2|public|\
             [(\"type_annotation\", \"double\")]"
            .to_string()],
        "#124.3: `using X = Y;` (alias_declaration) must surface as a TypeAlias"
    );
    assert!(
        refs_of(&refs, "Defines")
            .contains(&"a.cpp::geometry -> a.cpp::geometry::Distance".to_string()),
        "#124.3: the alias needs a Defines edge from its scope; got {:?}",
        refs_of(&refs, "Defines")
    );
}

/// `using namespace std;` / `using std::vector;` are `using_declaration`, a
/// DIFFERENT node kind that must keep its existing `Import` shaping — adding the
/// alias arm must not reroute them.
#[test]
fn cpp_using_declaration_stays_an_import() {
    let src = "using namespace std;\nusing std::vector;\n";
    let (nodes, _refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec![
            "Import|namespace std".to_string(),
            "Import|std::vector".to_string(),
        ],
        "#124.3: using-directives must stay Imports, not become TypeAliases"
    );
}

// ---- #124.4: data members are Fields with a type annotation -----------------

#[test]
fn cpp_data_members_are_fields_with_type_annotations() {
    let src = "class Shape {\n    int id;\n    double radius;\n    int a, b;\n\
               \x20   const char* p;\n    int& r;\n};\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        records_named(&nodes, "id"),
        vec!["Field|id|a.cpp::Shape::id|2|2|public|[(\"type_annotation\", \"int\")]".to_string()],
        "#124.4: a data member must be a Field (the flat C-family model), not a Constant"
    );
    // `int a, b;` is ONE field_declaration with TWO declarator fields — the
    // pre-fix single-name search dropped `a` entirely.
    assert_eq!(
        records_named(&nodes, "a"),
        vec!["Field|a|a.cpp::Shape::a|4|4|public|[(\"type_annotation\", \"int\")]".to_string()],
        "#124.4: every declarator of a multi-name member declaration must surface"
    );
    assert_eq!(
        records_named(&nodes, "b"),
        vec!["Field|b|a.cpp::Shape::b|4|4|public|[(\"type_annotation\", \"int\")]".to_string()],
        "#124.4: second declarator of `int a, b;`"
    );
    // Pointer and reference declarators unwrap to the bare member name.
    assert_eq!(
        records_named(&nodes, "p"),
        vec!["Field|p|a.cpp::Shape::p|5|5|public|[(\"type_annotation\", \"char\")]".to_string()],
        "#124.4: pointer_declarator must unwrap to the member name"
    );
    assert_eq!(
        records_named(&nodes, "r"),
        vec!["Field|r|a.cpp::Shape::r|6|6|public|[(\"type_annotation\", \"int\")]".to_string()],
        "#124.4: reference_declarator (fieldless) must unwrap to the member name"
    );
    assert_eq!(
        refs_of(&refs, "HasField"),
        vec![
            "a.cpp::Shape -> a.cpp::Shape::a".to_string(),
            "a.cpp::Shape -> a.cpp::Shape::b".to_string(),
            "a.cpp::Shape -> a.cpp::Shape::id".to_string(),
            "a.cpp::Shape -> a.cpp::Shape::p".to_string(),
            "a.cpp::Shape -> a.cpp::Shape::r".to_string(),
            "a.cpp::Shape -> a.cpp::Shape::radius".to_string(),
        ],
        "#124.4: each data member needs a HasField edge (not Defines)"
    );
}

/// One member declaration can bind names of DIFFERENT shapes (`int x, f();` —
/// a data member and a method prototype), so the prototype-vs-field decision is
/// per declarator. A node-wide test has to pick one answer for the whole
/// declaration, and either labels `x` a method or drops `f`.
#[test]
fn cpp_a_mixed_member_declaration_routes_each_name_separately() {
    let src = "class Mixed {\n    int x, f();\n};\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec![
            "Field|x".to_string(),
            "Method|f".to_string(),
            "Struct|Mixed".to_string(),
        ],
        "#124.4: each declared name is classified by its OWN declarator"
    );
    assert_eq!(
        refs_of(&refs, "HasField"),
        vec!["a.cpp::Mixed -> a.cpp::Mixed::x".to_string()]
    );
    assert_eq!(
        refs_of(&refs, "HasMethod"),
        vec!["a.cpp::Mixed -> a.cpp::Mixed::f#1".to_string()]
    );
}

// The prototype-vs-function-pointer decision (#135) is pinned in the sibling
// `cpp_member_classification_tests` module (split for the §4.1 file cap).

/// The #107 body-presence trap, in its C++ form: a nested forward declaration
/// (`class Inner;`) is a `field_declaration` whose `type` is a `class_specifier`
/// with NO `body` — the SAME node kind as a nested definition. Without the
/// body-presence guard it re-emits the referenced type as a second one-line
/// Struct node. It also carries no declarator, so it must contribute no Field.
#[test]
fn cpp_forward_declaration_in_a_class_body_emits_nothing() {
    let src = "class Outer {\n    class Inner;\n    struct Fwd;\n};\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec!["Struct|Outer".to_string()],
        "#124.4: a nested forward declaration must emit no Field and no duplicate Struct"
    );
    assert_eq!(
        refs_of(&refs, "HasField"),
        Vec::<String>::new(),
        "#124.4: a declarator-less member declaration has no field to attach"
    );
}

/// The same trap at FILE scope, found while building the guard above and fixed
/// under the boy-scout rule (§14): `class Shape;` followed by the real
/// `class Shape { … }` emitted TWO `Struct` nodes on the identical qualified name,
/// the first spanning only the declaration line. Verified against the pre-fix
/// build (both rows printed) before the guard moved into `emit_class_like`.
#[test]
fn cpp_forward_declaration_at_file_scope_emits_nothing() {
    let src = "class Shape;\nstruct S;\nenum class E : int;\nclass Shape { int x; };\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec!["Field|x".to_string(), "Struct|Shape".to_string()],
        "§14: a bodiless class/struct/enum specifier must emit nothing at file scope"
    );
    assert_eq!(
        records_named(&nodes, "Shape"),
        vec!["Struct|Shape|a.cpp::Shape|4|4|public|[(\"is_class\", \"true\")]".to_string()],
        "§14: exactly ONE node per qualified name, spanning the DEFINITION"
    );
    assert_eq!(
        refs_of(&refs, "Defines"),
        vec!["a.cpp -> a.cpp::Shape".to_string()],
        "§14: and exactly one Defines edge, not two"
    );
}

/// A nested type DEFINITION (`class Inner { int z; };`) is the same
/// `field_declaration`-wrapping-a-specifier shape WITH a body, so the guard
/// above must still let it through — otherwise reworking the member arm would
/// silently drop nested types (issue #107's `emit_inline_type` precedent).
#[test]
fn cpp_nested_type_definition_in_a_class_body_is_emitted() {
    let src = "class Outer {\n    class Inner { int z; };\n    enum E { A };\n};\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec![
            "Constant|A".to_string(),
            "Enum|E".to_string(),
            "Field|z".to_string(),
            "Struct|Inner".to_string(),
            "Struct|Outer".to_string(),
        ],
        "#124.4: a nested type definition and its members must survive the member rework"
    );
    assert!(
        refs_of(&refs, "Defines").contains(&"a.cpp::Outer -> a.cpp::Outer::Inner".to_string()),
        "#124.4: the nested type is defined by its enclosing class; got {:?}",
        refs_of(&refs, "Defines")
    );
}

// ---- #124.5: out-of-body definitions re-attach to their owner ---------------

#[test]
fn cpp_out_of_body_method_is_reattached_to_its_owner() {
    let src = "namespace geometry {\nclass Circle {\npublic:\n    double area() const;\n};\n}\n\
               double geometry::Circle::area() const { return 1.0; }\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        records_named(&nodes, "area"),
        vec![
            "Method|area|a.cpp::geometry::Circle::area#1|4|4|public|[(\"is_prototype\", \"true\"), \
             (\"receiver_type\", \"a.cpp::geometry::Circle\")]"
                .to_string(),
            "Method|area|a.cpp::geometry::Circle::area#2|7|7|public|\
             [(\"receiver_type\", \"a.cpp::geometry::Circle\")]"
                .to_string(),
        ],
        "#124.5: the out-of-body definition must be a Method under geometry::Circle, \
         not a file-scoped Function"
    );
    assert_eq!(
        refs_of(&refs, "HasMethod"),
        vec![
            "a.cpp::geometry::Circle -> a.cpp::geometry::Circle::area#1".to_string(),
            "a.cpp::geometry::Circle -> a.cpp::geometry::Circle::area#2".to_string(),
        ],
        "#124.5: the definition attaches to the qualified owner"
    );
}

/// An out-of-body constructor/destructor: the qualifier chain's innermost name
/// is an `identifier` (ctor) or a `destructor_name` (dtor), and the owner path
/// is every `scope` segment joined — verified against node-types.json
/// (`qualified_identifier` has `scope` + `name` fields, and nests).
#[test]
fn cpp_out_of_body_constructor_and_destructor_reattach() {
    let src = "namespace n { class C { public: C(int a); ~C(); }; }\n\
               n::C::C(int a) {}\nn::C::~C() {}\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        records_named(&nodes, "~C"),
        vec![
            "Method|~C|a.cpp::n::C::~C#2|1|1|public|[(\"is_prototype\", \"true\"), \
             (\"receiver_type\", \"a.cpp::n::C\")]"
                .to_string(),
            "Method|~C|a.cpp::n::C::~C#4|3|3|public|[(\"receiver_type\", \"a.cpp::n::C\")]"
                .to_string(),
        ],
        "#124.5: an out-of-body destructor keeps `~C` and re-attaches to n::C"
    );
    assert!(
        refs_of(&refs, "HasMethod").contains(&"a.cpp::n::C -> a.cpp::n::C::C#3".to_string()),
        "#124.5: an out-of-body constructor re-attaches to n::C; got {:?}",
        refs_of(&refs, "HasMethod")
    );
}

/// A definition WITHOUT a qualifier at file scope stays a `Function` + `Defines`
/// — the re-attachment must be driven by the qualifier, not applied to every
/// file-scope definition (negative assertion, §13.1 G4).
#[test]
fn cpp_unqualified_file_scope_definition_stays_a_function() {
    let src = "int freeFunction(int a) { return a; }\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        records_named(&nodes, "freeFunction"),
        vec!["Function|freeFunction|a.cpp::freeFunction#1|1|1|public|[]".to_string()],
        "#124.5: an unqualified definition must stay a file-scoped Function"
    );
    assert_eq!(
        refs_of(&refs, "Defines"),
        vec!["a.cpp -> a.cpp::freeFunction#1".to_string()],
        "#124.5: and keep its Defines edge (no HasMethod)"
    );
    assert_eq!(refs_of(&refs, "HasMethod"), Vec::<String>::new());
}
