// parser::spec::c_parity_tests — the exact-parity + per-clause assertions over
// the C ground truth in the sibling `c_ground_truth` module (which holds the
// corpus and the expected node/ref tables, split for the §4.1 file cap).
//

use std::collections::BTreeSet;

use super::c_ground_truth::{expected_node_records, expected_refs, CORPUS, PATH};
use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

fn node_record(n: &ExtractedNode) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{:?}",
        n.label, n.name, n.qualified_name, n.start_line, n.end_line, n.visibility, n.properties
    )
}

fn ref_triple(e: &ExtractedRef) -> (String, String, String) {
    (
        e.kind.clone(),
        e.from_qualified_name.clone(),
        e.to_qualified_name.clone(),
    )
}

fn parse() -> ParseResult {
    parse_file(CORPUS, PATH, Language::C).expect("c parse must not hard-fail")
}

#[test]
fn c_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean C must report 0 parse errors");

    let obs: BTreeSet<String> = r.nodes.iter().map(node_record).collect();
    let exp: BTreeSet<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        obs, exp,
        "C node set (full record) diverged from the hand-written walker's ground truth"
    );

    let obs_refs: BTreeSet<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp_refs: BTreeSet<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    assert_eq!(
        obs_refs, exp_refs,
        "C ref set diverged from the hand-written walker's ground truth"
    );
}

#[test]
fn c_per_edge_kind_f1_is_at_parity() {
    let r = parse();
    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();

    for kind in ["Defines", "HasMethod", "HasField", "Imports", "Calls"] {
        let exp_k: BTreeSet<(String, String)> = exp
            .iter()
            .filter(|(k, _, _)| k == kind)
            .map(|(_, f, t)| (f.clone(), t.clone()))
            .collect();
        let obs_k: BTreeSet<(String, String)> = obs_refs
            .iter()
            .filter(|(k, _, _)| k == kind)
            .map(|(_, f, t)| (f.clone(), t.clone()))
            .collect();
        let tp = exp_k.intersection(&obs_k).count();
        let fp = obs_k.difference(&exp_k).count();
        let fn_ = exp_k.difference(&obs_k).count();
        let precision = if tp + fp == 0 {
            1.0
        } else {
            tp as f64 / (tp + fp) as f64
        };
        let recall = if tp + fn_ == 0 {
            1.0
        } else {
            tp as f64 / (tp + fn_) as f64
        };
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        println!(
            "edge[{kind:<9}] P={precision:.3} R={recall:.3} F1={f1:.3} (tp={tp} fp={fp} fn={fn_})"
        );
        assert!(
            (f1 - 1.0).abs() < f64::EPSILON,
            "C {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}

/// Issue #106 regression: a C function is named by its DECLARATOR, never by a
/// parameter.
///
/// The parity corpus above covers the reported symptom, but only through one
/// signature shape. C hides the function name under several declarator
/// wrappers, and each is a distinct way for a name search to go wrong — so each
/// is pinned here rather than assumed to follow.
///
/// The `int f(void)` case is deliberately included even though it was always
/// correct: it is *why* the defect survived review. A fixture built from
/// parameterless functions cannot observe the bug at all.
#[test]
fn c_function_names_come_from_the_declarator_not_a_parameter() {
    let cases: &[(&str, &str, &str)] = &[
        // (source, expected function name, what it pins)
        (
            "int add(int a, int b) { return a + b; }",
            "add",
            "named parameters — the reported defect (returned `b`)",
        ),
        (
            "int add(int a, int b);",
            "add",
            "prototype with named parameters",
        ),
        (
            "int *make(int count) { return 0; }",
            "make",
            "pointer_declarator wrapper",
        ),
        (
            "char *dup(const char *src, int len) { return 0; }",
            "dup",
            "pointer return + multiple named parameters",
        ),
        (
            "int f(void) { return 0; }",
            "f",
            "no parameters — always worked, which masked the defect",
        ),
        (
            "static int scaled(int value) { return value; }",
            "scaled",
            "storage-class specifier before the declarator",
        ),
    ];

    for (src, expected, why) in cases {
        let r = parse_file(src, "t.c", Language::C).expect("c parse");
        let names: Vec<&str> = r
            .nodes
            .iter()
            .filter(|n| n.label == "Function")
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![*expected],
            "{why}: expected function named {expected:?} for {src:?}, got {names:?}"
        );
        // The QN must carry the same corrected name — a resolver looks the
        // symbol up by it, so a right name on a wrong QN would still be broken.
        let qns: Vec<&str> = r
            .nodes
            .iter()
            .filter(|n| n.label == "Function")
            .map(|n| n.qualified_name.as_str())
            .collect();
        assert!(
            qns.iter().all(|q| q.contains(&format!("::{expected}#"))),
            "{why}: QN must contain ::{expected}#, got {qns:?}"
        );
    }
}

/// Negative control for the test above: a parameter name must NOT become a
/// function name. Without this, a resolver that returned the FIRST identifier
/// anywhere would still pass the positive assertions whenever the function name
/// happens to sort first.
#[test]
fn c_parameter_names_never_become_function_names() {
    let r = parse_file(
        "int compute(int alpha, int omega) { return alpha; }",
        "t.c",
        Language::C,
    )
    .expect("c parse");
    let fn_names: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.label == "Function")
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(fn_names, vec!["compute"]);
    for param in ["alpha", "omega"] {
        assert!(
            !fn_names.contains(&param),
            "parameter {param:?} must never be emitted as a function name"
        );
    }
}

/// Issue #107 regression: preprocessor macros reach the graph.
///
/// The two shapes are deliberately different labels — an object-like `#define`
/// is a value, a function-like one is callable — so a consumer can tell them
/// apart, and `macro=true` marks both as preprocessor constructs rather than
/// real C objects.
#[test]
fn c_macros_are_extracted_with_a_macro_marker() {
    let r = parse_file(
        "#define MAX 10\n#define SQUARE(x) ((x)*(x))\n",
        "m.c",
        Language::C,
    )
    .expect("c parse");
    let find = |name: &str| r.nodes.iter().find(|n| n.name == name);

    let max = find("MAX").expect("object-like macro MAX must be extracted");
    assert_eq!(max.label, "Constant", "object-like macro is a value");
    assert!(
        max.properties
            .iter()
            .any(|(k, v)| k == "macro" && v == "true"),
        "MAX must be marked macro=true, got {:?}",
        max.properties
    );

    let sq = find("SQUARE").expect("function-like macro SQUARE must be extracted");
    assert_eq!(sq.label, "Function", "function-like macro is callable");
    assert!(
        sq.properties
            .iter()
            .any(|(k, v)| k == "macro" && v == "true"),
        "SQUARE must be marked macro=true, got {:?}",
        sq.properties
    );

    // A macro's replacement list is unexpanded tokens, not an expression, so no
    // Calls edge may be invented from it — `((x)*(x))` is not a call site.
    assert!(
        r.refs.iter().all(|e| e.kind != "Calls"),
        "a macro body must not produce Calls edges"
    );
}

/// Issue #107 regression: a struct DEFINED inline inside a typedef or a
/// declaration contributes its fields.
#[test]
fn c_inline_struct_definitions_contribute_their_fields() {
    let r = parse_file(
        "typedef struct { int ax; int ay; } Anon;\nstruct Tagged { int tv; } tagged_var;\n",
        "i.c",
        Language::C,
    )
    .expect("c parse");

    let structs: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.label == "Struct")
        .map(|n| n.name.as_str())
        .collect();
    assert!(
        structs.contains(&"Anon"),
        "an anonymous struct must be emitted under its typedef alias, got {structs:?}"
    );
    assert!(
        structs.contains(&"Tagged"),
        "a struct defined inline in a declaration must be emitted, got {structs:?}"
    );

    let fields: Vec<(&str, &str)> = r
        .nodes
        .iter()
        .filter(|n| n.label == "Field")
        .map(|n| (n.name.as_str(), n.qualified_name.as_str()))
        .collect();
    for (name, owner) in [("ax", "Anon"), ("ay", "Anon"), ("tv", "Tagged")] {
        assert!(
            fields
                .iter()
                .any(|(f, qn)| *f == name && qn.contains(&format!("::{owner}::"))),
            "field {name:?} must hang off {owner:?}; got {fields:?}"
        );
    }

    // The alias IS the struct for an anonymous body, so exactly one node may
    // own the QN — emitting both a Struct and a typedef Constant named `Anon`
    // would put two nodes on the same primary key.
    let anon_nodes: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.qualified_name.ends_with("::Anon"))
        .map(|n| n.label.as_str())
        .collect();
    assert_eq!(
        anon_nodes,
        vec!["Struct"],
        "exactly one node may own ::Anon, and it is the Struct; got {anon_nodes:?}"
    );
}

/// Negative control for the inline-type extraction: `typedef struct Point PointT;`
/// REFERENCES an existing struct — it does not define one — so it must not
/// re-emit `Point`.
///
/// An earlier draft of the #107 fix did exactly that, producing a duplicate
/// one-line `Point` node alongside the real definition. `struct_specifier` is
/// the same node kind for a definition and a reference; only the presence of a
/// `body` field distinguishes them.
#[test]
fn c_typedef_of_an_existing_struct_emits_no_duplicate() {
    let r = parse_file(
        "struct Point { int x; };\ntypedef struct Point PointT;\n",
        "d.c",
        Language::C,
    )
    .expect("c parse");
    let points: Vec<(&str, u64, u64)> = r
        .nodes
        .iter()
        .filter(|n| n.name == "Point" && n.label == "Struct")
        .map(|n| (n.name.as_str(), n.start_line, n.end_line))
        .collect();
    assert_eq!(
        points.len(),
        1,
        "`struct Point` must be emitted exactly once; a typedef REFERENCE is not \
         a second definition. Got {points:?}"
    );
    // And the typedef alias still exists in its own right.
    assert!(
        r.nodes
            .iter()
            .any(|n| n.name == "PointT" && n.label == "Constant"),
        "the typedef alias PointT must still be emitted"
    );
}
