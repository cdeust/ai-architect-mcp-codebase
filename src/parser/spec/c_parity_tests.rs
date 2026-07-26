// parser::spec::c_parity_tests — pins the C migration to the flat `clike`
// walker at EXACT parity with the hand-written walker it replaced
// (ADR-0055 phase 6, §5 step 3).
//
// The expected records below ARE the pre-migration `parse_c_file` output
// (full 7-tuple per node + full ref triples), captured mechanically from the
// hand-written walker on this corpus before it was deleted. Per-EdgeKind
// F1(new-vs-groundtruth) = 1.000 == F1(old-vs-groundtruth). The test parses
// through the crate's public `parse_file`, so it also covers the C dispatch arm.
//
// The corpus exercises every C concern the flat walker handles, plus the edge
// cases that pin specific behaviors (each preserved for parity, some documenting
// a pre-existing defect filed separately: naming #106, macros/inline-struct #107):
//   - `int add(int a, int b)` (def) and `int add(int a, int b);` (prototype):
//     both are named `add`. This is the ONE place this ground truth deliberately
//     DIVERGES from the deleted hand-written walker: its `find_identifier` LIFO-DFS
//     reached the parameter list before the declarator's own name and produced `b`
//     (the last parameter) for both. Issue #106 fixed that, so the expected values
//     below were updated by intent, not by rebaselining a failure. Every other row
//     is byte-identical to the pre-fix ground truth — the diff is exactly the two
//     Function rows plus the five CallSite/Calls QNs re-scoped under `add#3`.
//   - `int width, height;` — one field_declaration with TWO names; both surface.
//   - `char *name;` / `char buf[8];` / `int (*handler)(int, void *)` — pointer,
//     array, and function-pointer declarators all unwrap to the bare field name.
//   - `struct Point *next;` — the field's type_annotation is `struct Point`.
//   - `(fp)()` — a parenthesized (non-identifier) callee, DROPPED (no CallSite;
//     negative assertion). `obj.method` / `ptr->call` — member-access callees
//     resolve to the tail (`method` / `call`). `_internal()` — underscore callee kept.
//   - `#ifdef DEBUG … #endif` and `#if defined(FEATURE) … #endif` — the flat
//     walker recurses transparently through preprocessor wrappers, so `dbg`
//     (prototype), `Gated` (struct), and `gated` (function + call) inside them
//     are still extracted.
//   - `#define MAX 10` → `Constant` and `#define SQUARE(x) …` → `Function`,
//     both `macro=true` (issue #107). The deleted hand-written walker modelled
//     neither, so these rows are a DELIBERATE divergence from it, not a
//     rebaseline. No calls are scanned from a macro body: a replacement list is
//     unexpanded tokens, not an expression.
//   - `typedef struct { int ax; int ay; } Anon;` and
//     `struct Tagged { int tv; } tagged_var;` — a struct defined INLINE inside a
//     typedef or a declaration. Its body lives in the outer node's `type` field,
//     which the flat top-level scan never reached, so its FIELDS were invisible
//     (issue #107). `typedef struct Point PointT;` is the control: that is a
//     REFERENCE to an existing struct (no `body` field), and must NOT re-emit
//     `Point` — an earlier draft of the fix did exactly that and produced a
//     duplicate one-line `Point` node.
//   - `int global_var;` — a plain variable declaration, NOT a prototype (skipped).
//   - `int (*signal_handler)(int) = 0;` — a function-pointer variable WITH an
//     initializer: its `init_declarator` wraps a `function_declarator`, so
//     `is_c_function_prototype` classifies it as a prototype (`signal_handler#13`,
//     preserving the old walker's `init_declarator` branch — pins that branch).
//   - `int counter = 5;` — an initialized variable (an `init_declarator` with NO
//     function declarator) is NOT a prototype: it emits NOTHING (negative
//     assertion; both the old walker and the flat walker skip it, pinning the
//     inner `func_declarator` test inside the `init_declarator` branch).

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

const CORPUS: &str = r#"#include <stdio.h>
#include "config.h"
#include <sys/types.h>

#define MAX 10
#define SQUARE(x) ((x) * (x))

struct Point {
    int x;
    int y;
    int width, height;
    char *name;
    char buf[8];
    int (*handler)(int, void *);
    struct Point *next;
};

union Value {
    int i;
    float f;
};

enum Color {
    RED,
    GREEN = 5,
    BLUE
};

typedef struct Point PointT;
typedef unsigned long ulong_t;
typedef int (*Callback)(int);

int global_var;

int add(int a, int b);

static int helper(void) {
    return MAX;
}

int add(int a, int b) {
    int r = helper();
    printf("%d", r);
    obj.method(a);
    ptr->call(b);
    _internal();
    (fp)();
    return a + b;
}

void empty(void) {}

#ifdef DEBUG
int dbg(void);
#endif

#if defined(FEATURE)
struct Gated {
    int flag;
};
void gated(void) {
    dbg();
}
#endif

int (*signal_handler)(int) = 0;

int counter = 5;

typedef struct {
    int ax;
    int ay;
} Anon;

struct Tagged {
    int tv;
} tagged_var;
"#;

const PATH: &str = "app/main.c";

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

fn expected_node_records() -> Vec<&'static str> {
    vec![
        "CallSite|_internal|app/main.c::add#3::call@46:5#4|46|46|public|[(\"callee_name\", \"_internal\")]",
        "CallSite|call|app/main.c::add#3::call@45:5#5|45|45|public|[(\"callee_name\", \"call\")]",
        "CallSite|dbg|app/main.c::gated#11::call@62:5#12|62|62|public|[(\"callee_name\", \"dbg\")]",
        "CallSite|helper|app/main.c::add#3::call@42:13#8|42|42|public|[(\"callee_name\", \"helper\")]",
        "CallSite|method|app/main.c::add#3::call@44:5#6|44|44|public|[(\"callee_name\", \"method\")]",
        "CallSite|printf|app/main.c::add#3::call@43:5#7|43|43|public|[(\"callee_name\", \"printf\")]",
        "Constant|BLUE|app/main.c::Color::BLUE|26|26|public|[(\"enum_entry\", \"true\")]",
        // issue #107 — object-like macro
        "Constant|MAX|app/main.c::MAX|5|6|public|[(\"macro\", \"true\")]",
        // issue #107 — anonymous struct named by its typedef alias, with fields
        "Struct|Anon|app/main.c::Anon|70|73|public|[]",
        "Field|ax|app/main.c::Anon::ax|71|71|public|[(\"type_annotation\", \"int\")]",
        "Field|ay|app/main.c::Anon::ay|72|72|public|[(\"type_annotation\", \"int\")]",
        // issue #107 — struct defined inline in a declaration
        "Struct|Tagged|app/main.c::Tagged|75|77|public|[]",
        "Field|tv|app/main.c::Tagged::tv|76|76|public|[(\"type_annotation\", \"int\")]",
        "Constant|Callback|app/main.c::Callback|31|31|public|[(\"typedef\", \"true\")]",
        "Constant|GREEN|app/main.c::Color::GREEN|25|25|public|[(\"enum_entry\", \"true\")]",
        "Constant|PointT|app/main.c::PointT|29|29|public|[(\"typedef\", \"true\")]",
        "Constant|RED|app/main.c::Color::RED|24|24|public|[(\"enum_entry\", \"true\")]",
        "Constant|ulong_t|app/main.c::ulong_t|30|30|public|[(\"typedef\", \"true\")]",
        "Enum|Color|app/main.c::Color|23|27|public|[]",
        "Field|buf|app/main.c::Point::buf|13|13|public|[(\"type_annotation\", \"char\")]",
        "Field|f|app/main.c::Value::f|20|20|public|[(\"type_annotation\", \"float\")]",
        "Field|flag|app/main.c::Gated::flag|59|59|public|[(\"type_annotation\", \"int\")]",
        "Field|handler|app/main.c::Point::handler|14|14|public|[(\"type_annotation\", \"int\")]",
        "Field|height|app/main.c::Point::height|11|11|public|[(\"type_annotation\", \"int\")]",
        "Field|i|app/main.c::Value::i|19|19|public|[(\"type_annotation\", \"int\")]",
        "Field|name|app/main.c::Point::name|12|12|public|[(\"type_annotation\", \"char\")]",
        "Field|next|app/main.c::Point::next|15|15|public|[(\"type_annotation\", \"struct Point\")]",
        "Field|width|app/main.c::Point::width|11|11|public|[(\"type_annotation\", \"int\")]",
        "Field|x|app/main.c::Point::x|9|9|public|[(\"type_annotation\", \"int\")]",
        "Field|y|app/main.c::Point::y|10|10|public|[(\"type_annotation\", \"int\")]",
        // issue #107 — function-like macro
        "Function|SQUARE|app/main.c::SQUARE|6|7|public|[(\"macro\", \"true\")]",
        "Function|add|app/main.c::add#1|35|35|public|[(\"is_prototype\", \"true\")]",
        "Function|add|app/main.c::add#3|41|49|public|[]",
        "Function|dbg|app/main.c::dbg#10|54|54|public|[(\"is_prototype\", \"true\")]",
        "Function|empty|app/main.c::empty#9|51|51|public|[]",
        "Function|gated|app/main.c::gated#11|61|63|public|[]",
        "Function|helper|app/main.c::helper#2|37|39|public|[]",
        "Function|signal_handler|app/main.c::signal_handler#13|66|66|public|[(\"is_prototype\", \"true\")]",
        "Import|config.h|app/main.c::include:config.h|2|3|public|[(\"path\", \"config.h\")]",
        "Import|stdio.h|app/main.c::include:stdio.h|1|2|public|[(\"path\", \"stdio.h\")]",
        "Import|types.h|app/main.c::include:sys/types.h|3|4|public|[(\"path\", \"sys/types.h\")]",
        "Struct|Gated|app/main.c::Gated|58|60|public|[]",
        "Struct|Point|app/main.c::Point|8|16|public|[]",
        "Struct|Value|app/main.c::Value|18|21|public|[]",
    ]
}

fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Calls", "app/main.c::add#3", "_internal"),
        ("Calls", "app/main.c::add#3", "call"),
        ("Calls", "app/main.c::add#3", "helper"),
        ("Calls", "app/main.c::add#3", "method"),
        ("Calls", "app/main.c::add#3", "printf"),
        ("Calls", "app/main.c::gated#11", "dbg"),
        ("Defines", "app/main.c::Color", "app/main.c::Color::BLUE"),
        ("Defines", "app/main.c::Color", "app/main.c::Color::GREEN"),
        ("Defines", "app/main.c::Color", "app/main.c::Color::RED"),
        ("Defines", "app/main.c", "app/main.c::Callback"),
        ("Defines", "app/main.c", "app/main.c::Color"),
        ("Defines", "app/main.c", "app/main.c::Gated"),
        ("Defines", "app/main.c", "app/main.c::Point"),
        ("Defines", "app/main.c", "app/main.c::PointT"),
        ("Defines", "app/main.c", "app/main.c::Value"),
        ("Defines", "app/main.c", "app/main.c::add#1"),
        ("Defines", "app/main.c", "app/main.c::add#3"),
        ("Defines", "app/main.c", "app/main.c::dbg#10"),
        ("Defines", "app/main.c", "app/main.c::empty#9"),
        ("Defines", "app/main.c", "app/main.c::gated#11"),
        ("Defines", "app/main.c", "app/main.c::helper#2"),
        ("Defines", "app/main.c", "app/main.c::signal_handler#13"),
        ("Defines", "app/main.c", "app/main.c::ulong_t"),
        // issue #107 — macros and inline types
        ("Defines", "app/main.c", "app/main.c::MAX"),
        ("Defines", "app/main.c", "app/main.c::SQUARE"),
        ("Defines", "app/main.c", "app/main.c::Anon"),
        ("Defines", "app/main.c", "app/main.c::Tagged"),
        ("HasField", "app/main.c::Anon", "app/main.c::Anon::ax"),
        ("HasField", "app/main.c::Anon", "app/main.c::Anon::ay"),
        ("HasField", "app/main.c::Tagged", "app/main.c::Tagged::tv"),
        ("HasField", "app/main.c::Gated", "app/main.c::Gated::flag"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::buf"),
        (
            "HasField",
            "app/main.c::Point",
            "app/main.c::Point::handler",
        ),
        ("HasField", "app/main.c::Point", "app/main.c::Point::height"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::name"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::next"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::width"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::x"),
        ("HasField", "app/main.c::Point", "app/main.c::Point::y"),
        ("HasField", "app/main.c::Value", "app/main.c::Value::f"),
        ("HasField", "app/main.c::Value", "app/main.c::Value::i"),
        ("Imports", "app/main.c", "config.h"),
        ("Imports", "app/main.c", "stdio.h"),
        ("Imports", "app/main.c", "sys/types.h"),
    ]
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
