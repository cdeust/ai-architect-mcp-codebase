// parser::spec::cpp_member_classification_tests — the prototype-vs-data decision
// for a C++ class member (issue #135), split from `cpp_extraction_tests.rs` along
// the §4.1 concern boundary: `cpp_extraction_tests` pins the #123 NAMES and #124
// SHAPE gaps, this file pins WHICH label a member gets when its declarator is a
// function declarator — a `Method` (a real prototype) or a `Field` (a
// function-POINTER data member).
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

/// #135: a function-POINTER data member (`void (*cb)(int z);`) is a `Field`, not
/// a method prototype. Its outermost declarator IS a `function_declarator` (so
/// the pre-#135 whole-subtree discriminator called it a `Method`), but a
/// `pointer_declarator` sits BETWEEN that `function_declarator` and the name
/// (`function_declarator > parenthesized_declarator > pointer_declarator >
/// field_identifier`, verified against tree-sitter-cpp 0.23.4), which makes it a
/// pointer TO a function — data. It surfaces as `Field|cb` + `HasField` with the
/// declaration's `type` as its `type_annotation`.
///
/// Ground-truth delta (both directions), the #135 pin this replaces:
///   before: `Method|cb` (`is_prototype=true`) + `HasMethod`
///   after : `Field|cb`  (`type_annotation="void"`) + `HasField`
#[test]
fn cpp_function_pointer_member_is_a_field_not_a_method() {
    let src = "class Cb {\n    void (*cb)(int z);\n};\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec!["Field|cb".to_string(), "Struct|Cb".to_string()],
        "#135: a function-pointer member is data (Field), not a prototype Method"
    );
    assert_eq!(
        records_named(&nodes, "cb"),
        vec!["Field|cb|a.cpp::Cb::cb|2|2|public|[(\"type_annotation\", \"void\")]".to_string()],
        "#135: the Field carries the declaration's type as its annotation"
    );
    assert_eq!(
        refs_of(&refs, "HasField"),
        vec!["a.cpp::Cb -> a.cpp::Cb::cb".to_string()],
        "#135: the member attaches via HasField, not HasMethod"
    );
    assert_eq!(
        refs_of(&refs, "HasMethod"),
        Vec::<String>::new(),
        "#135: no method prototype is emitted for a function-pointer member"
    );
}

/// #135 negative control: a genuine member function prototype (`void f(int z);`)
/// and a pointer-RETURN prototype (`int *g();`) both STAY method prototypes. The
/// discriminator is position — a pointer/reference wrapper counts only BELOW the
/// outermost `function_declarator` (`(*cb)`), never ABOVE it (`*g()`, a pointer
/// return type). Without this control, a fix that demoted every
/// `function_declarator`-carrying member to a Field would still pass the test
/// above.
#[test]
fn cpp_real_prototype_and_pointer_return_stay_methods() {
    let src = "class K {\n    void f(int z);\n    int *g();\n};\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        label_names(&nodes),
        vec![
            "Method|f".to_string(),
            "Method|g".to_string(),
            "Struct|K".to_string()
        ],
        "#135: a real prototype and a pointer-return prototype are both Methods"
    );
    assert_eq!(
        refs_of(&refs, "HasField"),
        Vec::<String>::new(),
        "#135: neither is a data member"
    );
}
