// parser::spec::java_declarative_parity_tests — pins the declarative-
// conventions migration (issue #220 phase 2) to EXACT parity with the
// hand-written `JavaConventions` it supersedes in production.
//
// This repo already has `java_parity_tests.rs` (ADR-0055 walker→spec
// migration): it pins the CURRENT extraction against a FROZEN ground-truth
// transcript recorded from the pre-migration hand-written walker, which no
// longer exists to re-run. That test is untouched by this PR — it keeps
// proving the spec-table walkers haven't drifted from the original ADR-0055
// migration, independent of which `LanguageConventions` impl `JAVA_SPEC`
// wires up.
//
// This file is a SEPARATE, complementary oracle, in the pattern
// `go_parity_tests.rs` established for issue #220 phase 1: BOTH
// implementations under test are still live — `JAVA_SPEC` (production,
// `JavaDeclarativeConventions` backed by `JAVA_RULES`) and `JAVA_SPEC_LEGACY`
// (test-only, the hand-written `JavaConventions`, kept per the ADR rollback
// discipline). So this test diffs two REAL runs of the same corpus rather
// than a live run against a frozen transcript — a strictly stronger
// instrument for THIS migration: it cannot silently drift from what the
// hand-written implementation actually does today, and it automatically
// re-verifies on every change to either side. The two files therefore answer
// different questions and both are kept: `java_parity_tests.rs` answers "did
// the spec-table walker regress since ADR-0055"; this file answers "does the
// declarative engine reproduce `JavaConventions` exactly, today."
//
// "Identical means identical" (issue #220 task acceptance contract): same
// nodes (full 7-tuple record), same edges (full ref triple), same qualified
// names — checked as an exact set-equality `assert_eq!`, not a similarity
// score.
//
// The corpus below intentionally mirrors `java_parity_tests.rs`'s corpus (not
// imported — that module's items are private to its own `#[cfg(test)]`
// scope): both must exercise the same Java surface (modifier-keyword
// visibility on every access level, the `extends`+`implements` split, the
// `object_creation_expression` `type`-field fallback, plain and `static`
// imports) for their respective oracles to be meaningful.

use std::collections::BTreeSet;

use super::java::JAVA_SPEC_LEGACY;
use super::lang_spec::LangSpec;
use super::parse_with_spec;
use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

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

fn parse_declarative() -> ParseResult {
    // Production entry point: routes through `JAVA_SPEC`, whose `conventions`
    // is `JavaDeclarativeConventions` (issue #220 phase 2).
    parse_file(CORPUS, PATH, Language::Java).expect("java parse (declarative) must not hard-fail")
}

fn parse_legacy() -> ParseResult {
    // Test-only path: the same `LangSpec` data, wired to the hand-written
    // `JavaConventions` instead.
    let legacy: &LangSpec = &JAVA_SPEC_LEGACY;
    parse_with_spec(legacy, CORPUS, PATH).expect("java parse (legacy) must not hard-fail")
}

#[test]
fn java_declarative_and_legacy_conventions_agree_on_parse_errors() {
    let declarative = parse_declarative();
    let legacy = parse_legacy();
    assert_eq!(
        declarative.parse_errors, 0,
        "clean Java must report 0 parse errors"
    );
    assert_eq!(
        declarative.parse_errors, legacy.parse_errors,
        "declarative and legacy paths must report the same parse-error count"
    );
}

#[test]
fn java_declarative_conventions_are_at_exact_node_and_ref_parity_with_legacy() {
    let declarative = parse_declarative();
    let legacy = parse_legacy();

    let decl_nodes: BTreeSet<String> = declarative.nodes.iter().map(node_record).collect();
    let legacy_nodes: BTreeSet<String> = legacy.nodes.iter().map(node_record).collect();
    assert_eq!(
        decl_nodes, legacy_nodes,
        "declarative Java node set (full record) diverged from JavaConventions"
    );

    let decl_refs: BTreeSet<(String, String, String)> =
        declarative.refs.iter().map(ref_triple).collect();
    let legacy_refs: BTreeSet<(String, String, String)> =
        legacy.refs.iter().map(ref_triple).collect();
    assert_eq!(
        decl_refs, legacy_refs,
        "declarative Java ref set diverged from JavaConventions"
    );
}

#[test]
fn java_declarative_per_edge_kind_f1_is_at_parity_with_legacy() {
    let declarative = parse_declarative();
    let legacy = parse_legacy();
    let decl_refs: Vec<(String, String, String)> =
        declarative.refs.iter().map(ref_triple).collect();
    let legacy_refs: Vec<(String, String, String)> = legacy.refs.iter().map(ref_triple).collect();

    // The four structural edge kinds the accuracy gate scores, plus the
    // Java-specific Extends/Implements/HasVariant inheritance & enum edges.
    for kind in [
        "Defines",
        "HasMethod",
        "Imports",
        "Calls",
        "Extends",
        "Implements",
        "HasVariant",
    ] {
        let legacy_k: BTreeSet<(String, String)> = legacy_refs
            .iter()
            .filter(|(k, _, _)| k == kind)
            .map(|(_, f, t)| (f.clone(), t.clone()))
            .collect();
        let decl_k: BTreeSet<(String, String)> = decl_refs
            .iter()
            .filter(|(k, _, _)| k == kind)
            .map(|(_, f, t)| (f.clone(), t.clone()))
            .collect();
        let tp = legacy_k.intersection(&decl_k).count();
        let fp = decl_k.difference(&legacy_k).count();
        let fn_ = legacy_k.difference(&decl_k).count();
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
            "Java {kind} F1 {f1:.3} is below parity 1.000 (declarative vs. legacy)"
        );
    }
}

/// Sanity floor beneath the parity diff: proves the corpus actually exercises
/// the features it claims to (a parity test that diverged-and-both-sides-
/// stayed-empty would still pass `assert_eq!`), and directly checks the two
/// shapes THIS migration exists to prove (`VisibilityRule::ModifierKeyword`
/// via `node_visibility`, `ImportRule::StatementStrip`).
#[test]
fn java_declarative_conventions_actually_extract_the_corpus_features() {
    let r = parse_declarative();

    let has = |label: &str, name: &str| r.nodes.iter().any(|n| n.label == label && n.name == name);
    assert!(has("Trait", "Greeter"), "interface -> Trait");
    assert!(has("Trait", "Marker"), "annotation type -> Trait");
    assert!(has("Enum", "Color"));
    assert!(has("Variant", "RED"));
    assert!(has("Struct", "Dog"));
    assert!(has("Struct", "Point"), "record -> Struct");
    assert!(has("Constant", "name"));
    assert!(has("Import", "Config"));
    assert!(has("Import", "requireNonNull"), "static import");
    assert!(
        has("CallSite", "Config"),
        "object_creation_expression -> type-field fallback"
    );

    // `VisibilityRule::ModifierKeyword` via `node_visibility`: every access
    // level tree-sitter-java exposes, on independent node kinds.
    let vis_of = |label: &str, name: &str| {
        r.nodes
            .iter()
            .find(|n| n.label == label && n.name == name)
            .map(|n| n.visibility.clone())
    };
    assert_eq!(vis_of("Struct", "Dog"), Some("public".to_string()));
    assert_eq!(vis_of("Struct", "Animal"), Some("package".to_string()));
    assert_eq!(vis_of("Constant", "name"), Some("public".to_string()));
    assert_eq!(vis_of("Constant", "age"), Some("private".to_string()));
    assert_eq!(vis_of("Method", "helper"), Some("protected".to_string()));

    // `ImportRule::StatementStrip`: plain and `static` imports both shape to
    // their dotted path, quotes/`;`/keywords stripped.
    let cfg_import = r
        .nodes
        .iter()
        .find(|n| n.label == "Import" && n.name == "Config")
        .expect("plain import must be extracted");
    assert_eq!(
        cfg_import.properties,
        vec![("path".to_string(), "com.app.Config".to_string())]
    );
    let static_import = r
        .nodes
        .iter()
        .find(|n| n.label == "Import" && n.name == "requireNonNull")
        .expect("static import must be extracted");
    assert_eq!(
        static_import.properties,
        vec![(
            "path".to_string(),
            "java.util.Objects.requireNonNull".to_string()
        )]
    );

    // Split inheritance: `bases` + `implements` both present on `Dog`.
    let dog = r
        .nodes
        .iter()
        .find(|n| n.label == "Struct" && n.name == "Dog")
        .expect("Dog struct must be extracted");
    assert_eq!(
        dog.properties,
        vec![
            ("bases".to_string(), "Animal".to_string()),
            ("implements".to_string(), "Greeter,Walker".to_string()),
        ]
    );
}
