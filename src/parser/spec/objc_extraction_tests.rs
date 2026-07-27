// parser::spec::objc_extraction_tests — one targeted pin per acceptance clause of
// issues #127 (typedef of an inline struct) and #128 (keyword-selector names).
//
// The corpus-wide multiset assertion lives in `objc_parity_tests`; it proves the
// WHOLE output is right but says nothing about WHICH clause each row satisfies.
// These are the per-clause ledger, so a regression points at one clause instead of
// a 40-row diff. Every one was RUN against the pre-fix walker and failed; the
// failures are quoted in the PR.
//
// Every node kind and field named here was verified against tree-sitter-objc
// 3.0.2's src/node-types.json AND against the dumped AST — the selector shape in
// particular, because the grammar declares a `keyword_declarator` node that this
// version does not produce for either method form.
// source: tree-sitter-objc 3.0.2 src/node-types.json.

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language};

const PATH: &str = "a.m";

fn parse(src: &str) -> (Vec<ExtractedNode>, Vec<ExtractedRef>) {
    let r = parse_file(src, PATH, Language::ObjC).expect("objc parse must not hard-fail");
    assert_eq!(r.parse_errors, 0, "fixture must parse clean:\n{src}");
    (r.nodes, r.refs)
}

fn records(nodes: &[ExtractedNode]) -> Vec<String> {
    let mut out: Vec<String> = nodes
        .iter()
        .map(|n| {
            format!(
                "{}|{}|{}|{:?}",
                n.label, n.name, n.qualified_name, n.properties
            )
        })
        .collect();
    out.sort();
    out
}

fn names(nodes: &[ExtractedNode]) -> Vec<String> {
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

// ---- #127: a typedef's inline struct definition reaches the graph -----------

/// #127 acceptance clause 1, verbatim: "`typedef struct Node { int v; } NodeT;`
/// in a `.m`/`.h` emits `Struct|Node` + `Field|v` (`HasField`) AND
/// `Constant|NodeT`". The tag and the alias are two genuine names, so both
/// survive.
#[test]
fn objc_typedef_of_an_inline_struct_emits_the_struct_and_its_fields() {
    let (nodes, refs) = parse("typedef struct Node { int v; } NodeT;\n");
    assert_eq!(
        records(&nodes),
        vec![
            "Constant|NodeT|a.m::NodeT|[(\"typedef\", \"true\")]".to_string(),
            "Field|v|a.m::Node::v|[(\"type_annotation\", \"int\")]".to_string(),
            "Struct|Node|a.m::Node|[]".to_string(),
        ],
        "#127: the inline struct and its field were dropped entirely"
    );
    assert_eq!(
        refs_of(&refs, "HasField"),
        vec!["a.m::Node -> a.m::Node::v".to_string()],
        "#127: the field belongs to the tag"
    );
    assert_eq!(
        refs_of(&refs, "Defines"),
        vec![
            "a.m -> a.m::Node".to_string(),
            "a.m -> a.m::NodeT".to_string()
        ],
        "#127: both names are defined by the file"
    );
}

/// The ANONYMOUS half of #127's clause ("or the anonymous-inline shape #107
/// uses"): the alias is the only name that type has, so the struct is emitted
/// UNDER the alias and no separate alias `Constant` is emitted — two nodes on one
/// qualified name would be a duplicated primary key.
#[test]
fn objc_typedef_of_an_anonymous_inline_struct_uses_the_alias_once() {
    let (nodes, refs) = parse("typedef struct { int a; } AnonT;\n");
    assert_eq!(
        records(&nodes),
        vec![
            "Field|a|a.m::AnonT::a|[(\"type_annotation\", \"int\")]".to_string(),
            "Struct|AnonT|a.m::AnonT|[]".to_string(),
        ],
        "#127: the anonymous struct takes the alias name, and only once"
    );
    assert_eq!(
        refs_of(&refs, "Defines"),
        vec!["a.m -> a.m::AnonT".to_string()],
        "#127: exactly one Defines edge for the one name"
    );
}

/// The #107 body-presence guard, which is why the fix is a shared decision rather
/// than "emit whatever is in the `type` field": `typedef struct Node OtherT;`
/// REFERENCES an existing type. Re-emitting `Node` would put a second, one-line
/// `Struct` node on the real definition's qualified name.
#[test]
fn objc_typedef_of_an_existing_struct_emits_no_duplicate() {
    let (nodes, _refs) = parse("struct Node { int v; };\ntypedef struct Node OtherT;\n");
    assert_eq!(
        records(&nodes),
        vec![
            "Constant|OtherT|a.m::OtherT|[(\"typedef\", \"true\")]".to_string(),
            "Field|v|a.m::Node::v|[(\"type_annotation\", \"int\")]".to_string(),
            "Struct|Node|a.m::Node|[]".to_string(),
        ],
        "#127: exactly ONE Struct|Node, from the definition — not a second from the typedef"
    );
}

/// A typedef of an inline ENUM travels the same path, so its members surface too.
#[test]
fn objc_typedef_of_an_inline_enum_emits_the_enum_and_its_members() {
    let (nodes, refs) = parse("typedef enum Color { RED, GREEN } ColorT;\n");
    assert_eq!(
        names(&nodes),
        vec![
            "Constant|ColorT".to_string(),
            "Constant|GREEN".to_string(),
            "Constant|RED".to_string(),
            "Enum|Color".to_string(),
        ],
        "#127: the inline enum and its members surface under the tag"
    );
    assert!(
        refs_of(&refs, "Defines").contains(&"a.m::Color -> a.m::Color::RED".to_string()),
        "#127: members are defined by their enum; got {:?}",
        refs_of(&refs, "Defines")
    );
}

/// Boy-scout (§14), found while adding the guard above: a bodiless specifier is a
/// forward declaration, and emitting it put a SECOND `Struct` on the definition's
/// qualified name (measured on this exact input before the fix).
#[test]
fn objc_forward_declaration_emits_nothing() {
    let (nodes, refs) = parse("struct Point;\nenum E;\nstruct Point { int x; };\n");
    assert_eq!(
        names(&nodes),
        vec!["Field|x".to_string(), "Struct|Point".to_string()],
        "§14: a bodiless struct/enum specifier declares no type and emits nothing"
    );
    assert_eq!(
        refs_of(&refs, "Defines"),
        vec!["a.m -> a.m::Point".to_string()],
        "§14: exactly one Defines edge, not two"
    );
}

// ---- #128: a method's name is its FULL selector -----------------------------

/// #128 acceptance clause 1, verbatim: `- (int)areaWithWidth:(int)w
/// height:(int)h;` → `Method` named `areaWithWidth:height:`, with `HasMethod`.
#[test]
fn objc_keyword_selector_method_captures_every_keyword() {
    let src = "@interface Shape : NSObject\n\
               - (int)areaWithWidth:(int)w height:(int)h;\n\
               - (void)setX:(int)x y:(int)y z:(int)z;\n\
               @end\n";
    let (nodes, refs) = parse(src);
    assert_eq!(
        names(&nodes),
        vec![
            "Method|areaWithWidth:height:".to_string(),
            "Method|setX:y:z:".to_string(),
            "Struct|Shape".to_string(),
        ],
        "#128: every selector keyword, each followed by its colon"
    );
    assert_eq!(
        refs_of(&refs, "HasMethod"),
        vec![
            "a.m::Shape -> a.m::Shape::areaWithWidth:height:#1".to_string(),
            "a.m::Shape -> a.m::Shape::setX:y:z:#2".to_string(),
        ],
        "#128: keyed consistently on the full selector"
    );
}

/// A UNARY selector takes no argument, so it gets NO trailing colon — the
/// negative half of the rule (§13.1 G4). A rule that appended `:` per keyword
/// unconditionally would name this `draw:`.
#[test]
fn objc_unary_selector_method_has_no_colon() {
    let src = "@interface Shape : NSObject\n- (void)draw;\n@end\n";
    let (nodes, _refs) = parse(src);
    assert_eq!(
        names(&nodes),
        vec!["Method|draw".to_string(), "Struct|Shape".to_string()],
        "#128: a selector with no argument carries no colon"
    );
}

/// A single keyword that DOES take an argument gets exactly one colon
/// (`shapeNamed:`) — the case between the two above, and the one that shows the
/// colon tracks the argument rather than the keyword count.
#[test]
fn objc_single_keyword_selector_with_an_argument_keeps_its_colon() {
    let src = "@interface Shape : NSObject\n\
               + (instancetype)shapeNamed:(NSString *)name;\n\
               @end\n";
    let (nodes, _refs) = parse(src);
    assert_eq!(
        names(&nodes),
        vec!["Method|shapeNamed:".to_string(), "Struct|Shape".to_string()],
        "#128: one keyword with an argument → exactly one colon"
    );
}

/// The asymmetry #128 reports, closed: a method's name now equals what a message
/// SEND to it resolves to, so the `Calls` edge's target matches a declared
/// method's name and the resolver can connect them.
#[test]
fn objc_method_name_matches_what_a_message_send_resolves_to() {
    let src = "@interface Shape : NSObject\n\
               - (int)areaWithWidth:(int)w height:(int)h;\n\
               @end\n\
               @implementation Shape\n\
               - (void)go { [self areaWithWidth:1 height:2]; }\n\
               @end\n";
    let (nodes, refs) = parse(src);
    let declared: Vec<String> = nodes
        .iter()
        .filter(|n| n.label == "Method")
        .map(|n| n.name.clone())
        .collect();
    let called: Vec<String> = refs
        .iter()
        .filter(|r| r.kind == "Calls")
        .map(|r| r.to_qualified_name.clone())
        .collect();
    assert!(
        declared.contains(&"areaWithWidth:height:".to_string()),
        "#128: declaration side; got {declared:?}"
    );
    assert_eq!(
        called,
        vec!["areaWithWidth:height:".to_string()],
        "#128: the send resolves to the SAME selector the method is named by"
    );
}
