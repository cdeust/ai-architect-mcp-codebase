// parser::spec::kotlin_parity_tests — pins the Kotlin migration to the
// table-driven walkers at EXACT parity with the hand-written walker it replaced
// (ADR-0055 phase 4, §5 step 3).
//
// Before the old walker (`src/parser/kotlin/`) was deleted, a temporary
// equivalence check (tests/zzz_kotlin_capture.rs) recorded `parse_kotlin_file`'s
// output on this corpus node-for-node and ref-for-ref. That scaffold is gone
// with the old walker; this committed test is the durable ground-truth pin.
// The expected records below ARE the hand-written walker's exact output (full
// 7-tuple per node — label, name, qn, start/end line, visibility, ordered
// properties — and full ref triples), so per-EdgeKind F1(new-vs-groundtruth) =
// 1.0 == F1(old-vs-groundtruth).
//
// It parses through the crate's public `parse_file`, so it also covers the
// Kotlin dispatch arm and prints the per-EdgeKind precision/recall/F1 the PR
// parity table quotes.

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

// One Kotlin file exercising every concern the spec walker handles for Kotlin:
//   - `package` (skipped) and three imports (plain `import a.b.C`, aliased
//     `import a.b.C as D`, wildcard `import a.b.*`), shaped as `import:<path>`
//     QNs with a `path` property and a last-segment display name.
//   - an `interface` (`Greeter`) → `Trait` with its abstract method → `Method`/
//     `HasMethod`, and an `annotation class` (`Marker`) → `Struct`.
//   - an `enum class` (`Color`) → `Enum`, its entries (`RED`/`GREEN`/`BLUE`) →
//     `Constant` with `enum_entry=true`, reached via the `enum_class_body`.
//   - a `sealed class` (`Shape`) → `Struct` with a nested `class Circle : Shape()`
//     → `Struct` + `Extends`, and its method's call.
//   - a `data class` (`Point`) → `Struct` (its ctor params are not properties —
//     the name is one level below `first_identifier`, dropped by the walker).
//   - an `object` (`Registry`) → `Struct` with a method + a call; its `val`
//     property is dropped (name nested under `variable_declaration`).
//   - classes (`Animal`, `Dog`); `Dog : Animal(), Greeter` → two `Extends`
//     refs; `public` modifier visibility; a member extension `fun String.wag()`
//     and an override method.
//   - top-level functions (`topLevel`, extension `fun String.shout()`,
//     `useLambda`) → `Function`/`Defines`, keyed `name#seq`; a top-level `val`
//     (dropped); and calls incl. a chained `listOf(...).map { it * 2 }` and a
//     `this.uppercase()` navigation call reduced to its tail.
const CORPUS: &str = r#"package com.example.app

import kotlin.collections.List
import kotlin.math.max as maximum
import com.example.util.*

interface Greeter {
    fun greet(): String
}

annotation class Marker

enum class Color {
    RED,
    GREEN,
    BLUE
}

sealed class Shape {
    class Circle : Shape() {
        fun area(): Int {
            return compute()
        }
    }
}

data class Point(val x: Int, val y: Int)

object Registry {
    val instances: Int = 0
    fun register(): Int {
        return compute()
    }
}

class Animal {
    val species: String = "animal"
    fun breathe() {
        inhale()
    }
}

public class Dog : Animal(), Greeter {
    private val breed: String = "mutt"
    override fun greet(): String {
        return bark()
    }
    fun String.wag() {
        return
    }
}

fun topLevel(): Int {
    return helper()
}

fun String.shout(): String {
    return this.uppercase()
}

val VERSION: String = "1.0"

fun useLambda() {
    listOf(1, 2, 3).map { it * 2 }
}
"#;

const PATH: &str = "com/example/app/Demo.kt";

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
        "CallSite|bark|com/example/app/Demo.kt::Dog::greet#8::call@46:16#9|46|46|public|[(\"callee_name\", \"bark\")]",
        "CallSite|compute|com/example/app/Demo.kt::Registry::register#4::call@32:16#5|32|32|public|[(\"callee_name\", \"compute\")]",
        "CallSite|compute|com/example/app/Demo.kt::Shape::Circle::area#2::call@22:20#3|22|22|public|[(\"callee_name\", \"compute\")]",
        "CallSite|helper|com/example/app/Demo.kt::topLevel#11::call@54:12#12|54|54|public|[(\"callee_name\", \"helper\")]",
        "CallSite|inhale|com/example/app/Demo.kt::Animal::breathe#6::call@39:9#7|39|39|public|[(\"callee_name\", \"inhale\")]",
        "CallSite|listOf|com/example/app/Demo.kt::useLambda#15::call@64:5#17|64|64|public|[(\"callee_name\", \"listOf\")]",
        "CallSite|map|com/example/app/Demo.kt::useLambda#15::call@64:5#16|64|64|public|[(\"callee_name\", \"map\")]",
        "CallSite|uppercase|com/example/app/Demo.kt::shout#13::call@58:12#14|58|58|public|[(\"callee_name\", \"uppercase\")]",
        "Constant|BLUE|com/example/app/Demo.kt::Color::BLUE|16|16|public|[(\"enum_entry\", \"true\")]",
        "Constant|GREEN|com/example/app/Demo.kt::Color::GREEN|15|15|public|[(\"enum_entry\", \"true\")]",
        "Constant|RED|com/example/app/Demo.kt::Color::RED|14|14|public|[(\"enum_entry\", \"true\")]",
        "Enum|Color|com/example/app/Demo.kt::Color|13|17|public|[]",
        "Function|shout|com/example/app/Demo.kt::shout#13|57|59|public|[]",
        "Function|topLevel|com/example/app/Demo.kt::topLevel#11|53|55|public|[]",
        "Function|useLambda|com/example/app/Demo.kt::useLambda#15|63|65|public|[]",
        "Import|List|com/example/app/Demo.kt::import:kotlin.collections.List|3|3|public|[(\"path\", \"kotlin.collections.List\")]",
        "Import|max as maximum|com/example/app/Demo.kt::import:kotlin.math.max as maximum|4|4|public|[(\"path\", \"kotlin.math.max as maximum\")]",
        "Import||com/example/app/Demo.kt::import:com.example.util.*|5|5|public|[(\"path\", \"com.example.util.*\")]",
        "Method|area|com/example/app/Demo.kt::Shape::Circle::area#2|21|23|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Shape::Circle\")]",
        "Method|breathe|com/example/app/Demo.kt::Animal::breathe#6|38|40|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Animal\")]",
        "Method|greet|com/example/app/Demo.kt::Dog::greet#8|45|47|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Dog\")]",
        "Method|greet|com/example/app/Demo.kt::Greeter::greet#1|8|8|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Greeter\")]",
        "Method|register|com/example/app/Demo.kt::Registry::register#4|31|33|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Registry\")]",
        "Method|wag|com/example/app/Demo.kt::Dog::wag#10|48|50|public|[(\"receiver_type\", \"com/example/app/Demo.kt::Dog\")]",
        "Struct|Animal|com/example/app/Demo.kt::Animal|36|41|public|[]",
        "Struct|Circle|com/example/app/Demo.kt::Shape::Circle|20|24|public|[]",
        "Struct|Dog|com/example/app/Demo.kt::Dog|43|51|public|[]",
        "Struct|Marker|com/example/app/Demo.kt::Marker|11|11|public|[]",
        "Struct|Point|com/example/app/Demo.kt::Point|27|27|public|[]",
        "Struct|Registry|com/example/app/Demo.kt::Registry|29|34|public|[]",
        "Struct|Shape|com/example/app/Demo.kt::Shape|19|25|public|[]",
        "Trait|Greeter|com/example/app/Demo.kt::Greeter|7|9|public|[]",
    ]
}

fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Calls",
            "com/example/app/Demo.kt::Animal::breathe#6",
            "inhale",
        ),
        ("Calls", "com/example/app/Demo.kt::Dog::greet#8", "bark"),
        (
            "Calls",
            "com/example/app/Demo.kt::Registry::register#4",
            "compute",
        ),
        (
            "Calls",
            "com/example/app/Demo.kt::Shape::Circle::area#2",
            "compute",
        ),
        ("Calls", "com/example/app/Demo.kt::shout#13", "uppercase"),
        ("Calls", "com/example/app/Demo.kt::topLevel#11", "helper"),
        ("Calls", "com/example/app/Demo.kt::useLambda#15", "listOf"),
        ("Calls", "com/example/app/Demo.kt::useLambda#15", "map"),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Animal",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Color",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Dog",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Greeter",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Marker",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Point",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Registry",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::Shape",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::shout#13",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::topLevel#11",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt",
            "com/example/app/Demo.kt::useLambda#15",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt::Color",
            "com/example/app/Demo.kt::Color::BLUE",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt::Color",
            "com/example/app/Demo.kt::Color::GREEN",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt::Color",
            "com/example/app/Demo.kt::Color::RED",
        ),
        (
            "Defines",
            "com/example/app/Demo.kt::Shape",
            "com/example/app/Demo.kt::Shape::Circle",
        ),
        ("Extends", "com/example/app/Demo.kt::Dog", "Animal"),
        ("Extends", "com/example/app/Demo.kt::Dog", "Greeter"),
        ("Extends", "com/example/app/Demo.kt::Shape::Circle", "Shape"),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Animal",
            "com/example/app/Demo.kt::Animal::breathe#6",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Dog",
            "com/example/app/Demo.kt::Dog::greet#8",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Dog",
            "com/example/app/Demo.kt::Dog::wag#10",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Greeter",
            "com/example/app/Demo.kt::Greeter::greet#1",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Registry",
            "com/example/app/Demo.kt::Registry::register#4",
        ),
        (
            "HasMethod",
            "com/example/app/Demo.kt::Shape::Circle",
            "com/example/app/Demo.kt::Shape::Circle::area#2",
        ),
        ("Imports", "com/example/app/Demo.kt", "com.example.util.*"),
        (
            "Imports",
            "com/example/app/Demo.kt",
            "kotlin.collections.List",
        ),
        (
            "Imports",
            "com/example/app/Demo.kt",
            "kotlin.math.max as maximum",
        ),
    ]
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
