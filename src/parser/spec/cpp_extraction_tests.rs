// parser::spec::cpp_extraction_tests — targeted pins for how a C++ DECLARATOR is
// read: the name it binds (issue #123) and whether that name is a function or data
// (issue #135). The extraction-SHAPE pins for issue #124 — enum members,
// constructors, aliases, data members, out-of-body definitions — live in the
// sibling `cpp_shape_tests` module; the split is that concern boundary, and it is
// what holds both files under the 500-line cap (§4.1).
//
// The corpus-wide exact-set assertion lives in `cpp_parity_tests`; it proves the
// WHOLE output is right but says nothing about WHICH clause each row satisfies.
// These are the per-clause ledger, so a regression points at one clause instead of
// a 60-row set diff. Each was RUN against the pre-fix walker and failed (the
// failures are quoted in the PR).
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

/// A function-POINTER data member is DATA, not a method (issue #135).
///
/// `void (*cb)(int z);` and `void cb(int z);` share the same OUTERMOST
/// `function_declarator`, so the old "is there a `function_declarator` anywhere in
/// the subtree?" test called both methods. What distinguishes them is the
/// `pointer_declarator` between that function declarator and the name: `cb` is a
/// pointer TO a function, so it is a `Field`. The name is also right (`cb`, from
/// the declarator chain — it was `z`, the parameter, before #123).
#[test]
fn cpp_function_pointer_member_is_a_field_not_a_method() {
    let src = "class Cb {\n    void (*cb)(int z);\n    void handler(int z);\n};\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec![
            "Field|cb".to_string(),
            "Method|handler".to_string(),
            "Struct|Cb".to_string(),
        ],
        "#135: a pointer-to-function member is data; a plain declaration is a method"
    );
    assert_eq!(
        refs_of(&refs, "HasField"),
        vec!["a.cpp::Cb -> a.cpp::Cb::cb".to_string()],
        "#135: the function pointer attaches as a field"
    );
    assert_eq!(
        refs_of(&refs, "HasMethod"),
        vec!["a.cpp::Cb -> a.cpp::Cb::handler#1".to_string()],
        "#135: and only the real method as a method"
    );
}

/// The declarator readings that must NOT be swept up by #135's fix: a function
/// returning a pointer is still a function, and so is a function returning a
/// function pointer. Both put a `pointer_declarator` in the chain, so a naive
/// "any pointer ⇒ data" rule would misclassify them.
#[test]
fn cpp_function_returning_a_pointer_is_still_a_method() {
    let src = "class R {\n    int *make();\n    int (*factory(int n))(int);\n};\n";
    let (nodes, _refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec![
            "Method|factory".to_string(),
            "Method|make".to_string(),
            "Struct|R".to_string(),
        ],
        "#135: the pointer must be read as part of the RETURN type, not the name"
    );
}

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
