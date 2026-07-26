// parser::spec::swift_parity_tests — pins the Swift migration to the table-driven
// walkers at EXACT parity with the hand-written walker it replaced (ADR-0055
// phase 5, §5 step 4).
//
// Before the old walker (`src/parser/swift/`) was deleted, a temporary
// equivalence check (tests/zzz_swift_capture.rs) recorded `parse_swift_file`'s
// output on this corpus node-for-node and ref-for-ref (51 nodes, 51 refs, 0
// parse errors). That scaffold is gone with the old walker; this committed test
// is the durable ground-truth pin. The expected records below ARE the
// hand-written walker's exact output (full 7-tuple per node — label, name, qn,
// start/end line, visibility, ordered properties — and full ref triples), so
// per-EdgeKind F1(new-vs-groundtruth) = 1.000 == F1(old-vs-groundtruth).
//
// It parses through the crate's public `parse_file`, so it also covers the Swift
// dispatch arm and prints the per-EdgeKind precision/recall/F1 the PR parity
// table quotes.

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

// The comprehensive Swift corpus (also captured by tests/fixtures/. It exercises
// every concern the spec walker handles for Swift:
//   - three `import`s → `Import`/`Imports` with a `path` property.
//   - top-level `let`/`var` (`maxRetries`/`counter`) and a `fileprivate let`
//     (`sharedFlag`) → `Constant`s with modifier visibility.
//   - top-level `public func`/`private func` → `Function`/`Defines` keyed
//     `name#seq`, with their calls.
//   - a `struct` (`Point`) with stored `let`/`var` (→ `Constant`s), an `init`
//     (→ `Method` marked `member_kind=init`), a method, a computed `var`
//     (→ `Constant`, its body NOT call-scanned), and a `subscript`
//     (→ `Method` marked `member_kind=subscript`, its `computed_property` body
//     scanned for calls).
//   - a `public class` (`Animal`) with `init`/`deinit`/`open func` (open
//     visibility), and an `extension Animal` → a second `Struct` node with the
//     same QN marked `is_extension=true` whose method scopes under `::Animal`.
//   - an `actor` (`Counter`) → `Struct`.
//   - an `enum` (`Color`) → `Enum`, with `case red` / `case green, blue`
//     (one `enum_entry`, two variants) / `case custom(Int)` → `Variant`s edged
//     `Defines` with `internal` visibility, plus a method.
//   - a `protocol` (`Serializable`) → `Trait` (its requirement is a
//     `protocol_function_declaration`, which the hand-written walker did not
//     extract — preserved).
//   - a `typealias` → `Constant` marked `typealias=true`.
//   - calls incl. trailing closures (`items.map { transform($0) }`,
//     `fetch { … handle(…) }`), a qualified `Utils.parse()` reduced to `parse`,
//     and `self.reset()` reduced to `reset`.
const CORPUS: &str = include_str!("../../../tests/fixtures/swift_parity_corpus.swift");
const PATH: &str = "Sources/App/Demo.swift";

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
        "CallSite|at|Sources/App/Demo.swift::Point::subscript#9::call@36:16#10|36|36|internal|[(\"callee_name\", \"at\")]",
        "CallSite|bump|Sources/App/Demo.swift::Counter::increment#16::call@60:9#17|60|60|internal|[(\"callee_name\", \"bump\")]",
        "CallSite|cleanup|Sources/App/Demo.swift::Animal::deinit#12::call@48:9#13|48|48|internal|[(\"callee_name\", \"cleanup\")]",
        "CallSite|compute|Sources/App/Demo.swift::Point::distance#7::call@28:16#8|28|28|internal|[(\"callee_name\", \"compute\")]",
        "CallSite|compute|Sources/App/Demo.swift::helper#3::call@13:5#4|13|13|internal|[(\"callee_name\", \"compute\")]",
        "CallSite|convert|Sources/App/Demo.swift::Color::hex#18::call@70:16#19|70|70|internal|[(\"callee_name\", \"convert\")]",
        "CallSite|describe|Sources/App/Demo.swift::Point::init#5::call@24:22#6|24|24|internal|[(\"callee_name\", \"describe\")]",
        "CallSite|fetch|Sources/App/Demo.swift::useClosures#22::call@90:5#25|90|92|internal|[(\"callee_name\", \"fetch\")]",
        "CallSite|format|Sources/App/Demo.swift::greet#1::call@9:12#2|9|9|internal|[(\"callee_name\", \"format\")]",
        "CallSite|handle|Sources/App/Demo.swift::useClosures#22::call@91:9#26|91|91|internal|[(\"callee_name\", \"handle\")]",
        "CallSite|map|Sources/App/Demo.swift::useClosures#22::call@89:5#27|89|89|internal|[(\"callee_name\", \"map\")]",
        "CallSite|parse|Sources/App/Demo.swift::useClosures#22::call@93:5#24|93|93|internal|[(\"callee_name\", \"parse\")]",
        "CallSite|render|Sources/App/Demo.swift::Animal::describe#20::call@80:16#21|80|80|internal|[(\"callee_name\", \"render\")]",
        "CallSite|reset|Sources/App/Demo.swift::useClosures#22::call@94:5#23|94|94|internal|[(\"callee_name\", \"reset\")]",
        "CallSite|sound|Sources/App/Demo.swift::Animal::speak#14::call@52:16#15|52|52|internal|[(\"callee_name\", \"sound\")]",
        "CallSite|transform|Sources/App/Demo.swift::useClosures#22::call@89:17#28|89|89|internal|[(\"callee_name\", \"transform\")]",
        "Constant|Handler|Sources/App/Demo.swift::Handler|84|84|internal|[(\"typealias\", \"true\")]",
        "Constant|counter|Sources/App/Demo.swift::counter|6|6|internal|[]",
        "Constant|label|Sources/App/Demo.swift::Point::label|19|19|internal|[]",
        "Constant|magnitude|Sources/App/Demo.swift::Point::magnitude|31|33|internal|[]",
        "Constant|maxRetries|Sources/App/Demo.swift::maxRetries|5|5|internal|[]",
        "Constant|name|Sources/App/Demo.swift::Animal::name|41|41|internal|[]",
        "Constant|sharedFlag|Sources/App/Demo.swift::sharedFlag|86|86|fileprivate|[]",
        "Constant|value|Sources/App/Demo.swift::Counter::value|57|57|internal|[]",
        "Constant|x|Sources/App/Demo.swift::Point::x|17|17|internal|[]",
        "Constant|y|Sources/App/Demo.swift::Point::y|18|18|internal|[]",
        "Enum|Color|Sources/App/Demo.swift::Color|64|72|internal|[]",
        "Function|greet|Sources/App/Demo.swift::greet#1|8|10|public|[]",
        "Function|helper|Sources/App/Demo.swift::helper#3|12|14|private|[]",
        "Function|useClosures|Sources/App/Demo.swift::useClosures#22|88|95|internal|[]",
        "Import|Foundation|Sources/App/Demo.swift::import:Foundation|1|1|internal|[(\"path\", \"Foundation\")]",
        "Import|SwiftUI|Sources/App/Demo.swift::import:SwiftUI|3|3|internal|[(\"path\", \"SwiftUI\")]",
        "Import|UIKit|Sources/App/Demo.swift::import:UIKit|2|2|internal|[(\"path\", \"UIKit\")]",
        "Method|deinit|Sources/App/Demo.swift::Animal::deinit#12|47|49|internal|[(\"member_kind\", \"deinit\"), (\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|describe|Sources/App/Demo.swift::Animal::describe#20|79|81|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|distance|Sources/App/Demo.swift::Point::distance#7|27|29|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Point\")]",
        "Method|hex|Sources/App/Demo.swift::Color::hex#18|69|71|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Color\")]",
        "Method|increment|Sources/App/Demo.swift::Counter::increment#16|59|61|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Counter\")]",
        "Method|init|Sources/App/Demo.swift::Animal::init#11|43|45|internal|[(\"member_kind\", \"init\"), (\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|init|Sources/App/Demo.swift::Point::init#5|21|25|internal|[(\"member_kind\", \"init\"), (\"receiver_type\", \"Sources/App/Demo.swift::Point\")]",
        "Method|speak|Sources/App/Demo.swift::Animal::speak#14|51|53|open|[(\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|subscript|Sources/App/Demo.swift::Point::subscript#9|35|37|internal|[(\"member_kind\", \"subscript\"), (\"receiver_type\", \"Sources/App/Demo.swift::Point\")]",
        "Struct|Animal|Sources/App/Demo.swift::Animal|40|54|public|[]",
        "Struct|Animal|Sources/App/Demo.swift::Animal|78|82|internal|[(\"is_extension\", \"true\")]",
        "Struct|Counter|Sources/App/Demo.swift::Counter|56|62|internal|[]",
        "Struct|Point|Sources/App/Demo.swift::Point|16|38|internal|[]",
        "Trait|Serializable|Sources/App/Demo.swift::Serializable|74|76|internal|[]",
        "Variant|blue|Sources/App/Demo.swift::Color::blue|66|66|internal|[]",
        "Variant|custom|Sources/App/Demo.swift::Color::custom|67|67|internal|[]",
        "Variant|green|Sources/App/Demo.swift::Color::green|66|66|internal|[]",
        "Variant|red|Sources/App/Demo.swift::Color::red|65|65|internal|[]",
    ]
}

fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Calls",
            "Sources/App/Demo.swift::Animal::deinit#12",
            "cleanup",
        ),
        (
            "Calls",
            "Sources/App/Demo.swift::Animal::describe#20",
            "render",
        ),
        ("Calls", "Sources/App/Demo.swift::Animal::speak#14", "sound"),
        ("Calls", "Sources/App/Demo.swift::Color::hex#18", "convert"),
        (
            "Calls",
            "Sources/App/Demo.swift::Counter::increment#16",
            "bump",
        ),
        (
            "Calls",
            "Sources/App/Demo.swift::Point::distance#7",
            "compute",
        ),
        ("Calls", "Sources/App/Demo.swift::Point::init#5", "describe"),
        ("Calls", "Sources/App/Demo.swift::Point::subscript#9", "at"),
        ("Calls", "Sources/App/Demo.swift::greet#1", "format"),
        ("Calls", "Sources/App/Demo.swift::helper#3", "compute"),
        ("Calls", "Sources/App/Demo.swift::useClosures#22", "fetch"),
        ("Calls", "Sources/App/Demo.swift::useClosures#22", "handle"),
        ("Calls", "Sources/App/Demo.swift::useClosures#22", "map"),
        ("Calls", "Sources/App/Demo.swift::useClosures#22", "parse"),
        ("Calls", "Sources/App/Demo.swift::useClosures#22", "reset"),
        (
            "Calls",
            "Sources/App/Demo.swift::useClosures#22",
            "transform",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::name",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::blue",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::custom",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::green",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::red",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Counter",
            "Sources/App/Demo.swift::Counter::value",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::label",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::magnitude",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::x",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::y",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Animal",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Color",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Counter",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Handler",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Point",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::Serializable",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::counter",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::greet#1",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::helper#3",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::maxRetries",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::sharedFlag",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift",
            "Sources/App/Demo.swift::useClosures#22",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::deinit#12",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::describe#20",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::init#11",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::speak#14",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::hex#18",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Counter",
            "Sources/App/Demo.swift::Counter::increment#16",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::distance#7",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::init#5",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Point",
            "Sources/App/Demo.swift::Point::subscript#9",
        ),
        ("Imports", "Sources/App/Demo.swift", "Foundation"),
        ("Imports", "Sources/App/Demo.swift", "SwiftUI"),
        ("Imports", "Sources/App/Demo.swift", "UIKit"),
    ]
}

fn parse() -> ParseResult {
    parse_file(CORPUS, PATH, Language::Swift).expect("swift parse must not hard-fail")
}

#[test]
fn swift_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean Swift must report 0 parse errors");

    let obs: BTreeSet<String> = r.nodes.iter().map(node_record).collect();
    let exp: BTreeSet<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        obs, exp,
        "Swift node set (full record) diverged from the hand-written walker's ground truth"
    );

    let obs_refs: BTreeSet<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp_refs: BTreeSet<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    assert_eq!(
        obs_refs, exp_refs,
        "Swift ref set diverged from the hand-written walker's ground truth"
    );
}

#[test]
fn swift_per_edge_kind_f1_is_at_parity() {
    let r = parse();
    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();

    // The four structural edge kinds the accuracy gate scores. Swift declares no
    // inheritance edges (the hand-written walker never parsed conformance), so
    // there is no Extends/Implements row — its absence is itself parity.
    for kind in ["Defines", "HasMethod", "Imports", "Calls"] {
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
            "Swift {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}

// Node-level pins for edge-case constructs, each making a specific structural
// decision observable (§12 — the mutant it kills is named): an underscore-
// prefixed callee only the `c == '_'` char-class admits; a qualified callee
// reduced to its tail; a subscript whose body is scanned only via the
// `function_body_kinds` (`computed_property`) fallback; and a bodiless protocol
// requirement whose call in a default value is NOT scanned (the whole-node scan
// is confined to no-body-field grammars). Each behavior is the hand-written
// walker's, so these are parity pins too.
const EDGE_CORPUS: &str = r#"
protocol P {
    func need(x: Int) -> Int
}

struct Box {
    subscript(i: Int) -> Int {
        return _lookup(i)
    }
}

func edges() {
    Utils.parse()
    self._reset()
    obj.$binding()
}
"#;

fn has_call_to(r: &ParseResult, callee: &str) -> bool {
    r.refs
        .iter()
        .any(|e| e.kind == "Calls" && e.to_qualified_name == callee)
}

#[test]
fn swift_edgecase_callees_and_subscript_body() {
    let r = parse_file(EDGE_CORPUS, "Sources/Edge.swift", Language::Swift)
        .expect("swift edge parse must not hard-fail");
    assert_eq!(r.parse_errors, 0, "edge corpus must parse clean");

    // `_lookup(i)` inside a subscript's `computed_property` body is reached only
    // by the `function_body_kinds` fallback in `call_scan_of` (no `body` field on
    // subscript_declaration); the `_`-prefixed callee is admitted only by the
    // `c == '_'` char-class check. Kills both the fallback-removal and the
    // `== '_'` → `!= '_'` mutants.
    assert!(
        has_call_to(&r, "_lookup"),
        "underscore callee inside a subscript computed body must be scanned"
    );

    // `Utils.parse()` reduced to its tail `parse`; `self._reset()` to `_reset`.
    assert!(has_call_to(&r, "parse"), "qualified callee reduced to tail");
    assert!(
        has_call_to(&r, "_reset"),
        "self-qualified underscore callee reduced to tail"
    );

    // `obj.$binding()` — a property-wrapper projected-value callee reduces to the
    // tail `$binding`, whose leading `$` is neither alphabetic nor `_`, so the
    // hand-written walker (and this port) DROP it. Kills the `callee_tail`
    // `&&` → `||` mutant, which would keep any non-empty tail regardless of its
    // first character and wrongly emit a `Calls` edge to `$binding`.
    assert!(
        !has_call_to(&r, "$binding"),
        "dollar-prefixed projected-value callee must be dropped, not kept"
    );

    // The protocol requirement `func need(x: Int) -> Int` has no body; its
    // default-value-free signature yields no call, and — critically — the
    // whole-node fallback scan does NOT run for Swift (it has a named `body`
    // field), so `need` never appears as a spurious caller/call. Kills the mutant
    // that would extend the Kotlin whole-node fallback to Swift.
    assert!(
        !r.refs
            .iter()
            .any(|e| e.from_qualified_name.contains("::need")),
        "bodiless protocol requirement must scan no calls"
    );
}
