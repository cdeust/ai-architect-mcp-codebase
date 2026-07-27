// parser::spec::objc_extraction_tests — one targeted pin per acceptance clause
// of issues #127 (typedef-of-inline-struct) and #128 (keyword-selector names).
//
// The corpus-wide exact-multiset assertion lives in `objc_parity_tests`; it
// proves the WHOLE output is right but says nothing about WHICH clause each row
// satisfies. These tests are the per-clause ledger: each names the issue item it
// closes and asserts the minimum observable consequence, so a regression points
// at one clause instead of a set diff. Each was RUN against the pre-fix walker
// and failed (the failures are quoted in the PR's verification section).
//
// Every node kind and field named here is verified against tree-sitter-objc
// 3.0.2's src/node-types.json; the AST shapes were dumped from the grammar before
// the fix, not assumed.
// source: tree-sitter-objc 3.0.2 src/node-types.json.

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language};

const PATH: &str = "a.m";

fn parse(src: &str) -> (Vec<ExtractedNode>, Vec<ExtractedRef>) {
    let r = parse_file(src, PATH, Language::ObjC).expect("objc parse must not hard-fail");
    assert_eq!(r.parse_errors, 0, "fixture must parse clean:\n{src}");
    (r.nodes, r.refs)
}

/// The full 7-field record of every node with `name`, for an exact assertion
/// that also pins label/QN/lines/visibility/properties.
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

/// Every `(label, name)` pair emitted, sorted.
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

// ---- #127: typedef of an inline struct emits the struct and its fields -------

/// #127 acceptance clause 1: `typedef struct Node { int v; } NodeT;` emits
/// `Struct|Node` + `Field|v` (`HasField`) AND `Constant|NodeT`. The pre-fix
/// walker emitted ONLY `Constant|NodeT` — it never recursed the inline type in
/// the typedef's `type` field, so the struct and its field were silently dropped.
#[test]
fn objc_typedef_of_a_named_inline_struct_emits_the_struct_and_field() {
    let (nodes, refs) = parse("typedef struct Node { int v; } NodeT;\n");
    assert_eq!(
        label_names(&nodes),
        vec![
            "Constant|NodeT".to_string(),
            "Field|v".to_string(),
            "Struct|Node".to_string(),
        ],
        "#127: the inline struct and its field surface alongside the typedef alias"
    );
    assert_eq!(
        records_named(&nodes, "v"),
        vec!["Field|v|a.m::Node::v|1|1|public|[(\"type_annotation\", \"int\")]".to_string()],
        "#127: the field carries its type annotation, scoped under the struct tag"
    );
    assert_eq!(
        refs_of(&refs, "HasField"),
        vec!["a.m::Node -> a.m::Node::v".to_string()],
        "#127: the field attaches to the struct, not the alias"
    );
    assert_eq!(
        refs_of(&refs, "Defines"),
        vec![
            "a.m -> a.m::Node".to_string(),
            "a.m -> a.m::NodeT".to_string()
        ],
        "#127: both the tag and the alias are defined by the file"
    );
}

/// #127, the anonymous diagonal: `typedef struct { int v; } T;` has no tag, so
/// the struct is emitted under the ALIAS and no separate typedef `Constant` is
/// emitted (two nodes on one qualified name would be a duplicated key — the same
/// rule the C walker follows for `typedef struct { … } T;`).
#[test]
fn objc_typedef_of_an_anonymous_inline_struct_uses_the_alias_once() {
    let (nodes, refs) = parse("typedef struct { int v; } T;\n");
    assert_eq!(
        label_names(&nodes),
        vec!["Field|v".to_string(), "Struct|T".to_string()],
        "#127: the alias IS the struct — one node on a.m::T, and no typedef Constant"
    );
    assert_eq!(
        refs_of(&refs, "Defines"),
        vec!["a.m -> a.m::T".to_string()],
        "#127: exactly one Defines edge for the one name"
    );
}

/// #127 body-presence control: a typedef of an EXISTING struct
/// (`typedef struct Node NodeT;`, no body) must NOT re-emit `Node` — that would
/// duplicate the real definition's node. Only the alias `Constant` is emitted.
#[test]
fn objc_typedef_of_a_struct_reference_emits_only_the_alias() {
    let (nodes, _refs) = parse("typedef struct Node NodeT;\n");
    assert_eq!(
        label_names(&nodes),
        vec!["Constant|NodeT".to_string()],
        "#127: a bare reference (no body) emits no duplicate Struct"
    );
}

// ---- #128: a keyword-selector method name is the full selector --------------

/// #128 acceptance clause 1: `- (int)areaWithWidth:(int)w height:(int)h;` →
/// `Method` named `areaWithWidth:height:` (the FULL selector), not the first
/// keyword. A single-keyword selector keeps its colon (`shapeNamed:`), and a
/// unary selector stays bare (`start`) — matching how message SENDS already
/// reconstruct selectors.
#[test]
fn objc_keyword_selector_method_name_is_the_full_selector() {
    let src = "@interface S\n\
               - (int)areaWithWidth:(int)w height:(int)h;\n\
               + (id)shapeNamed:(id)n;\n\
               - (void)start;\n\
               @end\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        records_named(&nodes, "areaWithWidth:height:"),
        vec![
            "Method|areaWithWidth:height:|a.m::S::areaWithWidth:height:#1|2|2|public|\
             [(\"receiver_type\", \"a.m::S\")]"
                .to_string()
        ],
        "#128: a two-keyword selector joins both keywords with trailing colons"
    );
    assert_eq!(
        records_named(&nodes, "shapeNamed:"),
        vec![
            "Method|shapeNamed:|a.m::S::shapeNamed:#2|3|3|public|[(\"receiver_type\", \"a.m::S\")]"
                .to_string()
        ],
        "#128: a single-keyword selector keeps its colon"
    );
    assert_eq!(
        records_named(&nodes, "start"),
        vec![
            "Method|start|a.m::S::start#3|4|4|public|[(\"receiver_type\", \"a.m::S\")]".to_string()
        ],
        "#128: a unary selector stays bare (no colon)"
    );
    assert!(
        refs_of(&refs, "HasMethod")
            .contains(&"a.m::S -> a.m::S::areaWithWidth:height:#1".to_string()),
        "#128: the HasMethod edge keys on the full selector; got {:?}",
        refs_of(&refs, "HasMethod")
    );
}

/// #128 symmetry: a keyword method DEFINITION (with a body) resolves to the same
/// full selector as its declaration, and a message SEND to that selector already
/// did — so declaration, definition, and send now agree.
#[test]
fn objc_keyword_method_definition_matches_the_send_selector() {
    let src = "@implementation S\n\
               - (int)areaWithWidth:(int)w height:(int)h {\n\
               return [self compute:w];\n\
               }\n\
               @end\n";
    let (nodes, refs) = parse(src);
    assert!(
        label_names(&nodes).contains(&"Method|areaWithWidth:height:".to_string()),
        "#128: the definition resolves to the full selector; got {:?}",
        label_names(&nodes)
    );
    // The send inside it reconstructs `compute:` — the symmetry the fix restores.
    assert!(
        refs_of(&refs, "Calls")
            .contains(&"a.m::S::areaWithWidth:height:#1 -> compute:".to_string()),
        "#128: the enclosing method's calls key on the full selector; got {:?}",
        refs_of(&refs, "Calls")
    );
}
