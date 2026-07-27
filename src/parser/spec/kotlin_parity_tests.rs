// parser::spec::kotlin_parity_tests — pins the Kotlin migration to the
// table-driven walkers (ADR-0055 phase 4, §5 step 3).
//
// Before the old walker (`src/parser/kotlin/`) was deleted, a temporary
// equivalence check (tests/zzz_kotlin_capture.rs) recorded `parse_kotlin_file`'s
// output on this corpus node-for-node and ref-for-ref. That scaffold is gone
// with the old walker; this committed test is the durable ground-truth pin.
// The expected records below WERE the hand-written walker's exact output (full
// 7-tuple per node — label, name, qn, start/end line, visibility, ordered
// properties — and full ref triples), so per-EdgeKind F1(new-vs-groundtruth) =
// 1.0 == F1(old-vs-groundtruth) at migration time (PR #95).
//
// DELIBERATE DEVIATION FROM THE #95 CAPTURE (issue #93): the ground truth now
// ADDS the four `Constant`s + `Defines` that `property_declaration` names
// produce (top-level `VERSION`; members `Registry::instances`, `Animal::species`,
// `Dog::breed`). The hand-written walker dropped these — its `first_identifier`
// scanned only DIRECT `identifier` children, and a property's name lives one
// level down under `variable_declaration`. #95 preserved that drop for exact
// parity; this PR fixes it, so the pin is #95-parity **superseded for
// `property_declaration`**, not raw parity. Every other node/ref is unchanged
// from #95, and the diff to this file shows exactly those four nodes + four
// refs and nothing else.
//
// It parses through the crate's public `parse_file`, so it also covers the
// Kotlin dispatch arm and prints the per-EdgeKind precision/recall/F1 the PR
// parity table quotes.

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

use super::kotlin_ground_truth::{expected_node_records, expected_refs, CORPUS, PATH};

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
    parse_file(CORPUS, PATH, Language::Kotlin).expect("kotlin parse must not hard-fail")
}

#[test]
fn kotlin_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean Kotlin must report 0 parse errors");

    let obs: BTreeSet<String> = r.nodes.iter().map(node_record).collect();
    let exp: BTreeSet<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        obs, exp,
        "Kotlin node set (full record) diverged from the hand-written walker's ground truth"
    );

    let obs_refs: BTreeSet<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp_refs: BTreeSet<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    assert_eq!(
        obs_refs, exp_refs,
        "Kotlin ref set diverged from the hand-written walker's ground truth"
    );
}

#[test]
fn kotlin_per_edge_kind_f1_is_at_parity() {
    let r = parse();
    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();

    // The four structural edge kinds the accuracy gate scores, plus Kotlin's
    // Extends inheritance edges.
    for kind in ["Defines", "HasMethod", "Imports", "Calls", "Extends"] {
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
            "Kotlin {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}

// Node-level pins for edge-case constructs the main parity corpus does not
// exercise, chosen so each makes a specific structural decision observable
// (§12 — the mutant it kills is named): an expression-bodied function whose call
// is reachable only via the whole-node call scan; an `_`-prefixed callee that
// only the `c == '_'` char-class check admits; uppercase-object and package-
// qualified callees whose receiver `qualifier_or_tail` must preserve verbatim;
// and an empty `enum class` classified with no `enum_class_body` present. Each
// behavior is the hand-written walker's (the ported logic is byte-identical),
// so these are parity pins too.
const EDGE_CORPUS: &str = r#"package com.edge

enum class Empty

fun exprBodied(): Int = compute()

fun edges() {
    _init()
    Utils.parse()
    com.foo.Helper.build()
}
"#;

fn has_node(r: &ParseResult, label: &str, name: &str) -> bool {
    r.nodes.iter().any(|n| n.label == label && n.name == name)
}

fn has_call_to(r: &ParseResult, callee_repr: &str) -> bool {
    r.refs
        .iter()
        .any(|e| e.kind == "Calls" && e.to_qualified_name == callee_repr)
}

#[test]
fn kotlin_expression_body_and_edgecase_callees() {
    let r = parse_file(EDGE_CORPUS, "com/edge/Edge.kt", Language::Kotlin)
        .expect("kotlin edge parse must not hard-fail");
    assert_eq!(r.parse_errors, 0, "edge corpus must parse clean");

    // Empty `enum class` (no `enum_class_body`) still classifies as `Enum`
    // (kills the `classify_class` modifiers-check mutants; `enum class` → Enum,
    // and — cross-checked by `data class Point` in the main corpus staying a
    // Struct — the `&&` → `||` mutant that would make every modified class an
    // Enum).
    assert!(has_node(&r, "Enum", "Empty"), "empty enum class → Enum");

    // Expression-bodied `fun exprBodied() = compute()` has no `function_body`
    // child, so its call is found only by the whole-node fallback scan (kills the
    // `call_scan_of` `.or(Some(node))` removal mutant).
    assert!(
        has_call_to(&r, "compute"),
        "expression-bodied function's call must be scanned"
    );

    // `_init()` — an `_`-prefixed callee is admitted only by the `c == '_'`
    // char-class check (kills its `==` → `!=` mutant, which would drop the call).
    assert!(has_call_to(&r, "_init"), "underscore-prefixed callee kept");

    // `Utils.parse()` — uppercase-object single-dot qualifier preserved verbatim;
    // `com.foo.Helper.build()` — package-qualified (2+ dots) preserved verbatim.
    // Both pin `qualifier_or_tail`'s keep-branch at the node level.
    assert!(
        has_call_to(&r, "Utils.parse"),
        "uppercase object qualifier preserved"
    );
    assert!(
        has_call_to(&r, "com.foo.Helper.build"),
        "package-qualified callee preserved"
    );
}

// issue #93 fidelity: `property_declaration` names — silently dropped by the
// hand-written walker (and by #95, which preserved it) because the name is
// nested under a `variable_declaration` below `first_identifier`'s direct-child
// scan — now extract as `Constant` + `Defines`, for BOTH scopes (top-level and
// class member), with modifier-derived visibility, plus destructuring; while
// constructs that MUST stay dropped (constructor-property params, function-local
// `val`s) do stay dropped.
const PROPERTY_CORPUS: &str = r#"package com.props

const val TOP_LEVEL: Int = 1
var mutableTop = 2

class Holder {
    val publicField: String = "x"
    private val privateField: Int = 0
    var counter: Int = 0
}

object Config {
    val setting: Boolean = true
}

data class Pair2(val a: Int, val b: Int)

val (first, second) = makePair()

fun body() {
    val localOnly = 42
    localOnly.toString()
}
"#;

fn find_constant<'a>(r: &'a ParseResult, name: &str) -> Option<&'a ExtractedNode> {
    r.nodes
        .iter()
        .find(|n| n.label == "Constant" && n.name == name)
}

fn has_defines(r: &ParseResult, from: &str, to: &str) -> bool {
    r.refs
        .iter()
        .any(|e| e.kind == "Defines" && e.from_qualified_name == from && e.to_qualified_name == to)
}

#[test]
fn kotlin_property_declaration_names_extract() {
    let path = "com/props/Props.kt";
    let r = parse_file(PROPERTY_CORPUS, path, Language::Kotlin)
        .expect("kotlin property corpus must not hard-fail");
    assert_eq!(r.parse_errors, 0, "property corpus must parse clean");

    // --- top-level properties: name descended from `variable_declaration`, ---
    // --- defined by the FILE scope, visibility public (Kotlin default). ---
    let top = find_constant(&r, "TOP_LEVEL").expect("top-level `val` must emit a Constant (#93)");
    assert_eq!(top.qualified_name, format!("{path}::TOP_LEVEL"));
    assert_eq!(top.visibility, "public");
    assert!(top.properties.is_empty(), "property carries no enum marker");
    assert!(has_defines(&r, path, &format!("{path}::TOP_LEVEL")));
    // `var` is a property too.
    assert!(
        find_constant(&r, "mutableTop").is_some(),
        "top-level `var` must emit a Constant (#93)"
    );

    // --- class members: defined by the enclosing class, visibility from the ---
    // --- modifier (default public; `private` honored). ---
    let pubf =
        find_constant(&r, "publicField").expect("class-member `val` must emit a Constant (#93)");
    assert_eq!(pubf.qualified_name, format!("{path}::Holder::publicField"));
    assert_eq!(pubf.visibility, "public");
    assert!(has_defines(
        &r,
        &format!("{path}::Holder"),
        &format!("{path}::Holder::publicField")
    ));
    let privf = find_constant(&r, "privateField").expect("private member `val` must emit");
    assert_eq!(
        privf.visibility, "private",
        "private modifier must drive visibility (not the public default)"
    );
    assert!(
        find_constant(&r, "counter").is_some(),
        "class-member `var` must emit a Constant (#93)"
    );
    // Inside an `object` too.
    let setting = find_constant(&r, "setting").expect("object member `val` must emit");
    assert_eq!(setting.qualified_name, format!("{path}::Config::setting"));

    // --- destructuring `val (first, second) = …` → one Constant per name. ---
    assert!(
        find_constant(&r, "first").is_some() && find_constant(&r, "second").is_some(),
        "destructuring property must emit a Constant for EACH bound name (#93)"
    );
    assert!(has_defines(&r, path, &format!("{path}::first")));
    assert!(has_defines(&r, path, &format!("{path}::second")));

    // --- OUT OF SCOPE, must stay dropped: constructor-property params are ---
    // --- `class_parameter` nodes, not `property_declaration`. ---
    assert!(
        find_constant(&r, "a").is_none() && find_constant(&r, "b").is_none(),
        "data-class ctor params (`class_parameter`, not `property_declaration`) stay out of scope"
    );
    // --- function-local `val` is not a member/top-level property: `walk_defs` ---
    // --- never descends into function bodies, so it emits no Constant. ---
    assert!(
        find_constant(&r, "localOnly").is_none(),
        "function-local `val` must not become a Constant"
    );
}
