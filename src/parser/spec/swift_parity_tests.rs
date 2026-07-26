// parser::spec::swift_parity_tests — the Swift extraction ground-truth pin.
//
// Phase 5 (ADR-0055, PR #102) migrated Swift to the table-driven walkers at
// EXACT parity with the hand-written walker, deliberately PRESERVING four
// pre-existing extraction gaps and filing them as #97–#100. This PR closes those
// four gaps, so the ground truth below is the hand-written walker's output plus
// exactly those four behavior changes — nothing else:
//   - #97: superclass/protocol conformances now emit `Extends` edges (the corpus
//     `extension Animal: Equatable` → `Extends Animal -> Equatable`).
//   - #98: protocol requirements are extracted (`Serializable`'s `serialize` →
//     `Method` + `HasMethod`).
//   - #99: enum cases use the canonical `HasVariant`/`public` model (was
//     `Defines`/`internal`).
//   - #100: a computed property's accessor body is call-scanned (`Point`'s
//     `magnitude` getter `sqrt(x)` → `Calls Point::magnitude -> sqrt`).
// The two NEW seq-consuming emissions (the `serialize` method and the `sqrt`
// call) shift the per-file `#seq` counter, so every downstream `#seq` renumbers
// mechanically — the nodes/edges are otherwise identical, only their QN suffix
// changes. This is the parity-correct output: the hand-written walker, had it
// extracted these constructs, would have consumed the same counter.
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
//     (→ `Constant`, its getter body call-scanned → `Calls -> sqrt`; issue #100)
//     and a `subscript` (→ `Method` marked `member_kind=subscript`, its
//     `computed_property` body scanned for calls).
//   - a `public class` (`Animal`) with `init`/`deinit`/`open func` (open
//     visibility), and an `extension Animal: Equatable` → a second `Struct` node
//     with the same QN marked `is_extension=true` whose method scopes under
//     `::Animal`, plus an `Extends Animal -> Equatable` conformance edge (#97).
//   - an `actor` (`Counter`) → `Struct`.
//   - an `enum` (`Color`) → `Enum`, with `case red` / `case green, blue`
//     (one `enum_entry`, two variants) / `case custom(Int)` → `Variant`s edged
//     `HasVariant` with `public` visibility (the canonical model; issue #99),
//     plus a method.
//   - a `protocol` (`Serializable`) → `Trait` whose `serialize` requirement
//     (a `protocol_function_declaration`) is extracted as a `Method` +
//     `HasMethod`, bodiless so no calls are scanned (issue #98).
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
        "CallSite|at|Sources/App/Demo.swift::Point::subscript#10::call@36:16#11|36|36|internal|[(\"callee_name\", \"at\")]",
        "CallSite|bump|Sources/App/Demo.swift::Counter::increment#17::call@60:9#18|60|60|internal|[(\"callee_name\", \"bump\")]",
        "CallSite|cleanup|Sources/App/Demo.swift::Animal::deinit#13::call@48:9#14|48|48|internal|[(\"callee_name\", \"cleanup\")]",
        "CallSite|compute|Sources/App/Demo.swift::Point::distance#7::call@28:16#8|28|28|internal|[(\"callee_name\", \"compute\")]",
        "CallSite|compute|Sources/App/Demo.swift::helper#3::call@13:5#4|13|13|internal|[(\"callee_name\", \"compute\")]",
        "CallSite|convert|Sources/App/Demo.swift::Color::hex#19::call@70:16#20|70|70|internal|[(\"callee_name\", \"convert\")]",
        "CallSite|describe|Sources/App/Demo.swift::Point::init#5::call@24:22#6|24|24|internal|[(\"callee_name\", \"describe\")]",
        "CallSite|fetch|Sources/App/Demo.swift::useClosures#24::call@90:5#27|90|92|internal|[(\"callee_name\", \"fetch\")]",
        "CallSite|format|Sources/App/Demo.swift::greet#1::call@9:12#2|9|9|internal|[(\"callee_name\", \"format\")]",
        "CallSite|handle|Sources/App/Demo.swift::useClosures#24::call@91:9#28|91|91|internal|[(\"callee_name\", \"handle\")]",
        "CallSite|map|Sources/App/Demo.swift::useClosures#24::call@89:5#29|89|89|internal|[(\"callee_name\", \"map\")]",
        "CallSite|parse|Sources/App/Demo.swift::useClosures#24::call@93:5#26|93|93|internal|[(\"callee_name\", \"parse\")]",
        "CallSite|render|Sources/App/Demo.swift::Animal::describe#22::call@80:16#23|80|80|internal|[(\"callee_name\", \"render\")]",
        "CallSite|reset|Sources/App/Demo.swift::useClosures#24::call@94:5#25|94|94|internal|[(\"callee_name\", \"reset\")]",
        "CallSite|sound|Sources/App/Demo.swift::Animal::speak#15::call@52:16#16|52|52|internal|[(\"callee_name\", \"sound\")]",
        "CallSite|sqrt|Sources/App/Demo.swift::Point::magnitude::call@32:16#9|32|32|internal|[(\"callee_name\", \"sqrt\")]",
        "CallSite|transform|Sources/App/Demo.swift::useClosures#24::call@89:17#30|89|89|internal|[(\"callee_name\", \"transform\")]",
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
        "Function|useClosures|Sources/App/Demo.swift::useClosures#24|88|95|internal|[]",
        "Import|Foundation|Sources/App/Demo.swift::import:Foundation|1|1|internal|[(\"path\", \"Foundation\")]",
        "Import|SwiftUI|Sources/App/Demo.swift::import:SwiftUI|3|3|internal|[(\"path\", \"SwiftUI\")]",
        "Import|UIKit|Sources/App/Demo.swift::import:UIKit|2|2|internal|[(\"path\", \"UIKit\")]",
        "Method|deinit|Sources/App/Demo.swift::Animal::deinit#13|47|49|internal|[(\"member_kind\", \"deinit\"), (\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|describe|Sources/App/Demo.swift::Animal::describe#22|79|81|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|distance|Sources/App/Demo.swift::Point::distance#7|27|29|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Point\")]",
        "Method|hex|Sources/App/Demo.swift::Color::hex#19|69|71|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Color\")]",
        "Method|increment|Sources/App/Demo.swift::Counter::increment#17|59|61|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Counter\")]",
        "Method|init|Sources/App/Demo.swift::Animal::init#12|43|45|internal|[(\"member_kind\", \"init\"), (\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|init|Sources/App/Demo.swift::Point::init#5|21|25|internal|[(\"member_kind\", \"init\"), (\"receiver_type\", \"Sources/App/Demo.swift::Point\")]",
        "Method|serialize|Sources/App/Demo.swift::Serializable::serialize#21|75|75|internal|[(\"receiver_type\", \"Sources/App/Demo.swift::Serializable\")]",
        "Method|speak|Sources/App/Demo.swift::Animal::speak#15|51|53|open|[(\"receiver_type\", \"Sources/App/Demo.swift::Animal\")]",
        "Method|subscript|Sources/App/Demo.swift::Point::subscript#10|35|37|internal|[(\"member_kind\", \"subscript\"), (\"receiver_type\", \"Sources/App/Demo.swift::Point\")]",
        "Struct|Animal|Sources/App/Demo.swift::Animal|40|54|public|[]",
        "Struct|Animal|Sources/App/Demo.swift::Animal|78|82|internal|[(\"is_extension\", \"true\")]",
        "Struct|Counter|Sources/App/Demo.swift::Counter|56|62|internal|[]",
        "Struct|Point|Sources/App/Demo.swift::Point|16|38|internal|[]",
        "Trait|Serializable|Sources/App/Demo.swift::Serializable|74|76|internal|[]",
        "Variant|blue|Sources/App/Demo.swift::Color::blue|66|66|public|[]",
        "Variant|custom|Sources/App/Demo.swift::Color::custom|67|67|public|[]",
        "Variant|green|Sources/App/Demo.swift::Color::green|66|66|public|[]",
        "Variant|red|Sources/App/Demo.swift::Color::red|65|65|public|[]",
        ]
}

fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Calls",
            "Sources/App/Demo.swift::Animal::deinit#13",
            "cleanup",
        ),
        (
            "Calls",
            "Sources/App/Demo.swift::Animal::describe#22",
            "render",
        ),
        ("Calls", "Sources/App/Demo.swift::Animal::speak#15", "sound"),
        ("Calls", "Sources/App/Demo.swift::Color::hex#19", "convert"),
        (
            "Calls",
            "Sources/App/Demo.swift::Counter::increment#17",
            "bump",
        ),
        (
            "Calls",
            "Sources/App/Demo.swift::Point::distance#7",
            "compute",
        ),
        ("Calls", "Sources/App/Demo.swift::Point::init#5", "describe"),
        ("Calls", "Sources/App/Demo.swift::Point::magnitude", "sqrt"),
        ("Calls", "Sources/App/Demo.swift::Point::subscript#10", "at"),
        ("Calls", "Sources/App/Demo.swift::greet#1", "format"),
        ("Calls", "Sources/App/Demo.swift::helper#3", "compute"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "fetch"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "handle"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "map"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "parse"),
        ("Calls", "Sources/App/Demo.swift::useClosures#24", "reset"),
        (
            "Calls",
            "Sources/App/Demo.swift::useClosures#24",
            "transform",
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
            "Sources/App/Demo.swift::useClosures#24",
        ),
        (
            "Defines",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::name",
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
        ("Extends", "Sources/App/Demo.swift::Animal", "Equatable"),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::deinit#13",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::describe#22",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::init#12",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Animal",
            "Sources/App/Demo.swift::Animal::speak#15",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::hex#19",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Counter",
            "Sources/App/Demo.swift::Counter::increment#17",
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
            "Sources/App/Demo.swift::Point::subscript#10",
        ),
        (
            "HasMethod",
            "Sources/App/Demo.swift::Serializable",
            "Sources/App/Demo.swift::Serializable::serialize#21",
        ),
        (
            "HasVariant",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::blue",
        ),
        (
            "HasVariant",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::custom",
        ),
        (
            "HasVariant",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::green",
        ),
        (
            "HasVariant",
            "Sources/App/Demo.swift::Color",
            "Sources/App/Demo.swift::Color::red",
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

    // The structural edge kinds the accuracy gate scores. Swift now emits
    // `Extends` (conformance/inheritance, #97) and `HasVariant` (enum cases, #99)
    // in addition to the four base kinds, so both are scored here.
    for kind in [
        "Defines",
        "HasMethod",
        "Imports",
        "Calls",
        "Extends",
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
            "Swift {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}

// Node-level pins for edge-case constructs, each making a specific structural
// decision observable (§12 — the mutant it kills is named): an underscore-
// prefixed callee only the `c == '_'` char-class admits; a qualified callee
// reduced to its tail; a subscript whose body is scanned only via the
// `function_body_kinds` (`computed_property`) fallback; and a bodiless protocol
// requirement that IS extracted as a `Method` (#98) yet, having no body, scans
// no calls — and, critically, does NOT trigger the whole-node fallback scan
// (that is confined to no-body-field grammars like Kotlin).
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

    // The protocol requirement `func need(x: Int) -> Int` IS extracted as a
    // `Method` under `P` (#98): a `HasMethod P -> P::need` edge is present.
    assert!(
        r.refs
            .iter()
            .any(|e| e.kind == "HasMethod" && e.to_qualified_name.contains("::need")),
        "protocol function requirement must be extracted as a Method (#98)"
    );
    // But it has no body, so — critically — the whole-node fallback scan does NOT
    // run for Swift (it has a named `body` field): `need` never appears as a
    // spurious CALLER. Kills the mutant that would extend the Kotlin whole-node
    // fallback to Swift.
    assert!(
        !r.refs
            .iter()
            .any(|e| e.from_qualified_name.contains("::need")),
        "bodiless protocol requirement must scan no calls"
    );
}

// ---------------------------------------------------------------------------
// Fidelity tests for the four closed extraction gaps (#97–#100). Each asserts a
// previously-dropped construct now extracts, on a self-contained corpus so line
// and seq numbers stay local and legible.
// ---------------------------------------------------------------------------

fn parse_swift(src: &str, path: &str) -> ParseResult {
    let r = parse_file(src, path, Language::Swift).expect("swift parse must not hard-fail");
    assert_eq!(r.parse_errors, 0, "fidelity corpus must parse clean");
    r
}

fn has_extends(r: &ParseResult, from_suffix: &str, to: &str) -> bool {
    r.refs.iter().any(|e| {
        e.kind == "Extends"
            && e.from_qualified_name.ends_with(from_suffix)
            && e.to_qualified_name == to
    })
}

/// #97 — superclass, protocol conformance, protocol inheritance and extension
/// conformance all emit `Extends` edges (Swift, like Kotlin, does not split
/// superclass from conformance at parse time), while a clause-free type emits
/// none.
#[test]
fn swift_conformance_and_inheritance_edges() {
    let src = r#"
class Animal {}
protocol Serializable {}
class Dog: Animal, Serializable {
    func bark() {}
}
protocol Ordered: Serializable {}
extension Dog: Equatable {}
"#;
    let r = parse_swift(src, "Sources/Conf.swift");

    // `class Dog: Animal, Serializable` — the superclass AND the conformance are
    // BOTH `Extends` (no split; #97).
    assert!(
        has_extends(&r, "::Dog", "Animal"),
        "class superclass Extends missing"
    );
    assert!(
        has_extends(&r, "::Dog", "Serializable"),
        "class protocol-conformance Extends missing"
    );
    // Protocol inheritance (`protocol Ordered: Serializable`).
    assert!(
        has_extends(&r, "::Ordered", "Serializable"),
        "protocol inheritance Extends missing"
    );
    // Extension conformance (`extension Dog: Equatable`) — same QN as the class,
    // and the `is_extension=true` marker is preserved alongside the edge.
    assert!(
        has_extends(&r, "::Dog", "Equatable"),
        "extension conformance Extends missing"
    );
    assert!(
        r.nodes.iter().any(|n| n.label == "Struct"
            && n.name == "Dog"
            && n.properties
                .iter()
                .any(|(k, v)| k == "is_extension" && v == "true")),
        "extension node must still carry is_extension=true"
    );
    // A clause-free class emits NO Extends (kills a mutant that emits a spurious
    // edge for every class).
    assert!(
        !r.refs
            .iter()
            .any(|e| e.kind == "Extends" && e.from_qualified_name.ends_with("::Animal")),
        "a class with no inheritance clause must emit no Extends"
    );
}

/// #98 — protocol function requirements are extracted as `Method` + `HasMethod`
/// and protocol property requirements as `Constant` + `Defines`, both under the
/// `Trait`; bodiless requirements scan no calls.
#[test]
fn swift_protocol_requirements_extracted() {
    let src = r#"
protocol Repository {
    var id: String { get }
    func fetch(key: String) -> Int
    func save(item: Int)
}
"#;
    let r = parse_swift(src, "Sources/Repo.swift");

    for name in ["fetch", "save"] {
        assert!(
            r.nodes
                .iter()
                .any(|n| n.label == "Method" && n.name == name),
            "protocol func requirement `{name}` not extracted as a Method"
        );
        assert!(
            r.refs.iter().any(|e| e.kind == "HasMethod"
                && e.to_qualified_name
                    .contains(&format!("::Repository::{name}#"))),
            "protocol func requirement `{name}` missing HasMethod edge"
        );
    }
    // Property requirement `var id: String { get }` → Constant + Defines.
    assert!(
        r.nodes
            .iter()
            .any(|n| n.label == "Constant" && n.name == "id"),
        "protocol property requirement `id` not extracted as a Constant"
    );
    assert!(
        r.refs
            .iter()
            .any(|e| e.kind == "Defines" && e.to_qualified_name.ends_with("::Repository::id")),
        "protocol property requirement `id` missing Defines edge"
    );
    // Every requirement is bodiless → no call is scanned anywhere.
    assert!(
        !r.refs.iter().any(|e| e.kind == "Calls"),
        "bodiless protocol requirements must scan no calls"
    );
}

/// #99 — enum cases use the canonical `HasVariant`/`public` model (Java's
/// `enum_constant` model), not the removed `Defines`/`internal` one.
#[test]
fn swift_enum_cases_are_hasvariant_public() {
    let src = "enum Suit {\n    case hearts, spades\n    case clubs\n}\n";
    let r = parse_swift(src, "Sources/Suit.swift");

    for name in ["hearts", "spades", "clubs"] {
        let v = r
            .nodes
            .iter()
            .find(|n| n.label == "Variant" && n.name == name)
            .unwrap_or_else(|| panic!("variant `{name}` missing"));
        assert_eq!(
            v.visibility, "public",
            "enum case `{name}` must be public (#99)"
        );
        assert!(
            r.refs.iter().any(|e| e.kind == "HasVariant"
                && e.to_qualified_name.ends_with(&format!("::Suit::{name}"))),
            "enum case `{name}` must be edged HasVariant (#99)"
        );
    }
    // The old `Defines`-to-a-case edge model is gone.
    assert!(
        !r.refs
            .iter()
            .any(|e| e.kind == "Defines" && e.to_qualified_name.contains("::Suit::hearts")),
        "enum case must NOT be edged Defines (old model removed)"
    );
}

/// #100 — a computed property's accessor body (implicit-return getter, explicit
/// get/set, and stored-property willSet/didSet observers) is scanned for calls,
/// keyed by the property's QN; a plain stored property scans nothing.
#[test]
fn swift_computed_property_bodies_are_call_scanned() {
    let src = r#"
struct Vec2 {
    var stored: Int = 0
    var magnitude: Double {
        return sqrt(sumSquares())
    }
    var label: String {
        get { return format() }
        set { validate(newValue) }
    }
    var observed: Int = 0 {
        didSet { recompute() }
    }
}
"#;
    let r = parse_swift(src, "Sources/Vec2.swift");

    let calls_from = |prop: &str, callee: &str| {
        r.refs.iter().any(|e| {
            e.kind == "Calls"
                && e.from_qualified_name.ends_with(&format!("::{prop}"))
                && e.to_qualified_name == callee
        })
    };
    // Implicit-return computed getter — both the outer and nested call scanned.
    assert!(
        calls_from("magnitude", "sqrt"),
        "computed getter call `sqrt` unscanned"
    );
    assert!(
        calls_from("magnitude", "sumSquares"),
        "nested call in computed getter unscanned"
    );
    // Explicit get + set accessors both scanned.
    assert!(
        calls_from("label", "format"),
        "explicit getter call unscanned"
    );
    assert!(
        calls_from("label", "validate"),
        "explicit setter call unscanned"
    );
    // Stored-property observer (`didSet`) scanned.
    assert!(
        calls_from("observed", "recompute"),
        "didSet observer call unscanned"
    );
    // A plain stored property (no accessor) scans nothing — the asymmetry #100
    // closes must not over-correct into scanning stored initializers.
    assert!(
        !r.refs
            .iter()
            .any(|e| e.kind == "Calls" && e.from_qualified_name.ends_with("::stored")),
        "plain stored property must scan no calls"
    );
}
