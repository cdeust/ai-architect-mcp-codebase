// parser::spec::java_parity_tests — pins the Java migration to the table-driven
// walkers at EXACT parity with the hand-written walker it replaced (ADR-0055
// phase 3, §5 step 3).
//
// Before the old walker (`src/parser/java/`) was deleted, a temporary
// equivalence check (tests/zzz_java_capture.rs) recorded `parse_java_file`'s
// output on this corpus node-for-node and ref-for-ref. That scaffold is gone
// with the old walker; this committed test is the durable ground-truth pin.
// The expected records below ARE the hand-written walker's exact output (full
// 7-tuple per node — label, name, qn, start/end line, visibility, ordered
// properties — and full ref triples), so per-EdgeKind F1(new-vs-groundtruth) =
// 1.0 == F1(old-vs-groundtruth).
//
// It parses through the crate's public `parse_file`, so it also covers the Java
// dispatch arm and prints the per-EdgeKind precision/recall/F1 the PR parity
// table quotes.

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

// One Java file exercising every concern the spec walker handles for Java:
//   - `package` (skipped) and two imports (plain `import a.b.C;` + `import
//     static a.b.C.m;`), shaped as `import:<path>` QNs with a `path` property.
//   - interfaces (`Greeter`, `Walker`) and an annotation (`@interface Marker`)
//     → `Trait`, with their abstract methods → `Method`/`HasMethod`.
//   - an `enum` (`Color`) → `Enum`, its constants (`RED`/`GREEN`/`BLUE`) →
//     `Variant`/`HasVariant`, and its `enum_body_declarations` members (a
//     method + a field) reached through the transparent body wrapper.
//   - classes (`Animal`, `Dog`) → `Struct`; `Dog extends Animal implements
//     Greeter, Walker` → split inheritance: a `bases` property + `Extends`
//     ref AND an `implements` property + two `Implements` refs.
//   - a `record` (`Point`) → `Struct` (its header params are not fields).
//   - fields (`name` public, `age` private, `KIND` package-default) → `Constant`
//     with modifier-keyword visibility.
//   - a constructor (`Dog`) and methods (`greet` public, `walk` public,
//     `helper` protected) → `Method`/`HasMethod`, keyed `name#seq`.
//   - call sites: a bare invocation (`requireNonNull`), a method invocation
//     (`helper`), and an `object_creation_expression` (`new Config()` → callee
//     from the `type` field) → `CallSite` + `Calls`.
const CORPUS: &str = r#"package com.app;

import com.app.Config;
import static java.util.Objects.requireNonNull;

interface Greeter {
    String greet();
}

interface Walker {
    void walk();
}

@interface Marker {
}

enum Color {
    RED, GREEN, BLUE;
    void shade() {}
    int code = 0;
}

class Animal {
    void breathe() {}
}

public class Dog extends Animal implements Greeter, Walker {
    public String name;
    private int age;
    static final String KIND = "canine";

    public Dog(String name) {
        this.name = name;
        requireNonNull(name);
    }

    public String greet() {
        Config c = new Config();
        helper();
        return "woof";
    }

    public void walk() {}

    protected void helper() {}
}

record Point(int x, int y) {}
"#;

const PATH: &str = "com/app/Demo.java";

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
        "CallSite|Config|com/app/Demo.java::Dog::greet#7::call@38:20#9|38|38|package|[(\"callee_name\", \"Config\"), (\"lsp_col\", \"19\")]",
        "CallSite|helper|com/app/Demo.java::Dog::greet#7::call@39:9#8|39|39|package|[(\"callee_name\", \"helper\"), (\"lsp_col\", \"8\")]",
        "CallSite|requireNonNull|com/app/Demo.java::Dog::Dog#5::call@34:9#6|34|34|package|[(\"callee_name\", \"requireNonNull\"), (\"lsp_col\", \"8\")]",
        "Constant|KIND|com/app/Demo.java::Dog::KIND|30|30|package|[]",
        "Constant|age|com/app/Demo.java::Dog::age|29|29|private|[]",
        "Constant|code|com/app/Demo.java::Color::code|20|20|package|[]",
        "Constant|name|com/app/Demo.java::Dog::name|28|28|public|[]",
        "Enum|Color|com/app/Demo.java::Color|17|21|package|[]",
        "Import|Config|com/app/Demo.java::import:com.app.Config|3|3|package|[(\"path\", \"com.app.Config\")]",
        "Import|requireNonNull|com/app/Demo.java::import:java.util.Objects.requireNonNull|4|4|package|[(\"path\", \"java.util.Objects.requireNonNull\")]",
        "Method|Dog|com/app/Demo.java::Dog::Dog#5|32|35|public|[(\"receiver_type\", \"com/app/Demo.java::Dog\")]",
        "Method|breathe|com/app/Demo.java::Animal::breathe#4|24|24|package|[(\"receiver_type\", \"com/app/Demo.java::Animal\")]",
        "Method|greet|com/app/Demo.java::Dog::greet#7|37|41|public|[(\"receiver_type\", \"com/app/Demo.java::Dog\")]",
        "Method|greet|com/app/Demo.java::Greeter::greet#1|7|7|package|[(\"receiver_type\", \"com/app/Demo.java::Greeter\")]",
        "Method|helper|com/app/Demo.java::Dog::helper#11|45|45|protected|[(\"receiver_type\", \"com/app/Demo.java::Dog\")]",
        "Method|shade|com/app/Demo.java::Color::shade#3|19|19|package|[(\"receiver_type\", \"com/app/Demo.java::Color\")]",
        "Method|walk|com/app/Demo.java::Dog::walk#10|43|43|public|[(\"receiver_type\", \"com/app/Demo.java::Dog\")]",
        "Method|walk|com/app/Demo.java::Walker::walk#2|11|11|package|[(\"receiver_type\", \"com/app/Demo.java::Walker\")]",
        "Struct|Animal|com/app/Demo.java::Animal|23|25|package|[]",
        "Struct|Dog|com/app/Demo.java::Dog|27|46|public|[(\"bases\", \"Animal\"), (\"implements\", \"Greeter,Walker\")]",
        "Struct|Point|com/app/Demo.java::Point|48|48|package|[]",
        "Trait|Greeter|com/app/Demo.java::Greeter|6|8|package|[]",
        "Trait|Marker|com/app/Demo.java::Marker|14|15|package|[]",
        "Trait|Walker|com/app/Demo.java::Walker|10|12|package|[]",
        "Variant|BLUE|com/app/Demo.java::Color::BLUE|18|18|public|[]",
        "Variant|GREEN|com/app/Demo.java::Color::GREEN|18|18|public|[]",
        "Variant|RED|com/app/Demo.java::Color::RED|18|18|public|[]",
    ]
}

fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut v = Vec::new();
    v.extend(expected_refs_part0());
    v.extend(expected_refs_part1());
    v.extend(expected_refs_part2());
    v.extend(expected_refs_part3());
    v
}

fn expected_refs_part0() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Calls", "com/app/Demo.java::Dog::Dog#5", "requireNonNull"),
        ("Calls", "com/app/Demo.java::Dog::greet#7", "Config"),
        ("Calls", "com/app/Demo.java::Dog::greet#7", "helper"),
        (
            "Defines",
            "com/app/Demo.java::Color",
            "com/app/Demo.java::Color::code",
        ),
        (
            "Defines",
            "com/app/Demo.java::Dog",
            "com/app/Demo.java::Dog::KIND",
        ),
        (
            "Defines",
            "com/app/Demo.java::Dog",
            "com/app/Demo.java::Dog::age",
        ),
        (
            "Defines",
            "com/app/Demo.java::Dog",
            "com/app/Demo.java::Dog::name",
        ),
        ("Defines", "com/app/Demo.java", "com/app/Demo.java::Animal"),
        ("Defines", "com/app/Demo.java", "com/app/Demo.java::Color"),
        ("Defines", "com/app/Demo.java", "com/app/Demo.java::Dog"),
        ("Defines", "com/app/Demo.java", "com/app/Demo.java::Greeter"),
        ("Defines", "com/app/Demo.java", "com/app/Demo.java::Marker"),
        ("Defines", "com/app/Demo.java", "com/app/Demo.java::Point"),
        ("Defines", "com/app/Demo.java", "com/app/Demo.java::Walker"),
    ]
}

fn expected_refs_part1() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Extends", "com/app/Demo.java::Dog", "Animal"),
        (
            "HasMethod",
            "com/app/Demo.java::Animal",
            "com/app/Demo.java::Animal::breathe#4",
        ),
        (
            "HasMethod",
            "com/app/Demo.java::Color",
            "com/app/Demo.java::Color::shade#3",
        ),
        (
            "HasMethod",
            "com/app/Demo.java::Dog",
            "com/app/Demo.java::Dog::Dog#5",
        ),
        (
            "HasMethod",
            "com/app/Demo.java::Dog",
            "com/app/Demo.java::Dog::greet#7",
        ),
        (
            "HasMethod",
            "com/app/Demo.java::Dog",
            "com/app/Demo.java::Dog::helper#11",
        ),
    ]
}

fn expected_refs_part2() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "HasMethod",
            "com/app/Demo.java::Dog",
            "com/app/Demo.java::Dog::walk#10",
        ),
        (
            "HasMethod",
            "com/app/Demo.java::Greeter",
            "com/app/Demo.java::Greeter::greet#1",
        ),
        (
            "HasMethod",
            "com/app/Demo.java::Walker",
            "com/app/Demo.java::Walker::walk#2",
        ),
        (
            "HasVariant",
            "com/app/Demo.java::Color",
            "com/app/Demo.java::Color::BLUE",
        ),
        (
            "HasVariant",
            "com/app/Demo.java::Color",
            "com/app/Demo.java::Color::GREEN",
        ),
        (
            "HasVariant",
            "com/app/Demo.java::Color",
            "com/app/Demo.java::Color::RED",
        ),
    ]
}

fn expected_refs_part3() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Implements", "com/app/Demo.java::Dog", "Greeter"),
        ("Implements", "com/app/Demo.java::Dog", "Walker"),
        ("Imports", "com/app/Demo.java", "com.app.Config"),
        (
            "Imports",
            "com/app/Demo.java",
            "java.util.Objects.requireNonNull",
        ),
    ]
}

fn parse() -> ParseResult {
    parse_file(CORPUS, PATH, Language::Java).expect("java parse must not hard-fail")
}

#[test]
fn java_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean Java must report 0 parse errors");

    let obs: BTreeSet<String> = r.nodes.iter().map(node_record).collect();
    let exp: BTreeSet<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        obs, exp,
        "Java node set (full record) diverged from the hand-written walker's ground truth"
    );

    let obs_refs: BTreeSet<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp_refs: BTreeSet<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    assert_eq!(
        obs_refs, exp_refs,
        "Java ref set diverged from the hand-written walker's ground truth"
    );
}

#[test]
fn java_per_edge_kind_f1_is_at_parity() {
    let r = parse();
    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();

    // The four structural edge kinds the accuracy gate scores, plus the Java-
    // specific Extends/Implements/HasVariant inheritance & enum edges.
    for kind in [
        "Defines",
        "HasMethod",
        "Imports",
        "Calls",
        "Extends",
        "Implements",
        "HasVariant",
    ] {
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
            "edge[{kind:<10}] P={precision:.3} R={recall:.3} F1={f1:.3} (tp={tp} fp={fp} fn={fn_})"
        );
        assert!(
            (f1 - 1.0).abs() < f64::EPSILON,
            "Java {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}
