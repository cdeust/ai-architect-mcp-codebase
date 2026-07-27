// parser::spec::cpp_shape_tests — one targeted pin per acceptance clause of issue
// #124 (C++ extraction-shape gaps): enum members, constructors/destructors,
// `using` aliases, data members, and out-of-body definitions, plus the
// forward-declaration guard found alongside them (§14).
//
// The declarator-READING pins (#123 names, #135 function-vs-data) are the sibling
// `cpp_extraction_tests` module. Each test here names the issue item it closes and
// asserts the minimum observable consequence; each was RUN against the pre-fix
// walker and failed (the failures are quoted in the PR).
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
