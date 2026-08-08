// parser::spec::go_parity_tests — pins the declarative-conventions migration
// (issue #220 phase 1) to EXACT parity with the hand-written `GoConventions`
// it supersedes in production.
//
// Unlike `c_parity_tests.rs` / `java_parity_tests.rs` / `swift_parity_tests.rs`
// (which hardcode a ground truth transcribed from a walker that no longer
// exists), BOTH implementations here are still live: `GO_SPEC` (production,
// `DeclarativeConventions(&GO_RULES)`) and `GO_SPEC_LEGACY` (test-only,
// `GoConventions`, kept per the ADR rollback discipline). So this test diffs
// two REAL runs of the same corpus rather than a live run against a frozen
// transcript — a strictly stronger instrument: it cannot silently drift from
// what the hand-written implementation actually does today, and it
// automatically re-verifies on every change to either side.
//
// "Identical means identical" (issue #220 task acceptance contract): same
// nodes (full 7-tuple record), same edges (full ref triple), same qualified
// names — checked as an exact set-equality `assert_eq!`, not a similarity
// score.

use std::collections::BTreeSet;

use super::go::GO_SPEC_LEGACY;
use super::lang_spec::LangSpec;
use super::parse_with_spec;
use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

/// One Go file exercising every concern `GO_RULES` (`go.rs`) covers:
///   - `package` (skipped).
///   - two imports: a single `import "fmt"` and a grouped
///     `import ( "strings" "sync" )` — both descend through `import_spec`.
///   - an exported struct (`Widget`) and an unexported one (`gadget`), each
///     with fields → `Field`/`HasField`.
///   - an interface type (`Shaper`) → `Trait` via `interface_type_kind`.
///   - a type alias (`Label = string`).
///   - package-level exported/unexported const and var declarations,
///     including a grouped `var` block (multi-name DFS, `value_name_field:
///     None`).
///   - a free function (`NewWidget`) returning `Widget` — exercises
///     `return_type_field: Some("result")` — and calling `fmt.Println`
///     (dotted callee → tail segment `Println`) and a bare call
///     (`helper()`).
///   - a pointer-receiver method (`(w *Widget) Area`) and a value-receiver
///     method (`(g gadget) Name`) — exercises `receiver_type` (strip parens,
///     last token, strip leading `*`).
///   - a composite-literal construction (`Widget{}`) inside a function body
///     — exercises `type_construction_kinds`.
///   - a call whose callee is a non-identifier expression (`(fp)()`) — must
///     be DROPPED (the `require_leading_ident` gate), so a divergence in
///     the gate logic between the two conventions would show up as a set
///     mismatch, not just a missing test case.
const CORPUS: &str = r#"package widget

import "fmt"
import (
	"strings"
	"sync"
)

type Label = string

type Shaper interface {
	Area() float64
}

type Widget struct {
	Width  float64
	Height float64
}

type gadget struct {
	name string
}

const MaxWidgets = 10
const minWidgets = 1

var (
	DefaultLabel Label = "default"
	counter      int
)

func helper() {}

func NewWidget(w, h float64) Widget {
	fmt.Println("creating widget")
	helper()
	strings.TrimSpace("x")
	sync.OnceFunc(helper)
	return Widget{Width: w, Height: h}
}

func (w *Widget) Area() float64 {
	return w.Width * w.Height
}

func (g gadget) Name() string {
	return g.name
}

func dropped() {
	var fp func()
	fp()
	(fp)()
}
"#;

const PATH: &str = "widget/widget.go";

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
    // Production entry point: routes through `GO_SPEC`, whose `conventions`
    // is `DeclarativeConventions(&GO_RULES)` (issue #220 phase 1).
    parse_file(CORPUS, PATH, Language::Go).expect("go parse (declarative) must not hard-fail")
}

fn parse_legacy() -> ParseResult {
    // Test-only path: the same `LangSpec` data, wired to the hand-written
    // `GoConventions` instead.
    let legacy: &LangSpec = &GO_SPEC_LEGACY;
    parse_with_spec(legacy, CORPUS, PATH).expect("go parse (legacy) must not hard-fail")
}

#[test]
fn go_declarative_and_legacy_conventions_agree_on_parse_errors() {
    let declarative = parse_declarative();
    let legacy = parse_legacy();
    assert_eq!(
        declarative.parse_errors, 0,
        "clean Go must report 0 parse errors"
    );
    assert_eq!(
        declarative.parse_errors, legacy.parse_errors,
        "declarative and legacy paths must report the same parse-error count"
    );
}

#[test]
fn go_declarative_conventions_are_at_exact_node_and_ref_parity_with_legacy() {
    let declarative = parse_declarative();
    let legacy = parse_legacy();

    let decl_nodes: BTreeSet<String> = declarative.nodes.iter().map(node_record).collect();
    let legacy_nodes: BTreeSet<String> = legacy.nodes.iter().map(node_record).collect();
    assert_eq!(
        decl_nodes, legacy_nodes,
        "declarative Go node set (full record) diverged from GoConventions"
    );

    let decl_refs: BTreeSet<(String, String, String)> =
        declarative.refs.iter().map(ref_triple).collect();
    let legacy_refs: BTreeSet<(String, String, String)> =
        legacy.refs.iter().map(ref_triple).collect();
    assert_eq!(
        decl_refs, legacy_refs,
        "declarative Go ref set diverged from GoConventions"
    );
}

#[test]
fn go_declarative_per_edge_kind_f1_is_at_parity_with_legacy() {
    let declarative = parse_declarative();
    let legacy = parse_legacy();
    let decl_refs: Vec<(String, String, String)> =
        declarative.refs.iter().map(ref_triple).collect();
    let legacy_refs: Vec<(String, String, String)> = legacy.refs.iter().map(ref_triple).collect();

    for kind in ["Defines", "HasMethod", "HasField", "Imports", "Calls"] {
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
            "edge[{kind:<9}] P={precision:.3} R={recall:.3} F1={f1:.3} (tp={tp} fp={fp} fn={fn_})"
        );
        assert!(
            (f1 - 1.0).abs() < f64::EPSILON,
            "Go {kind} F1 {f1:.3} is below parity 1.000 (declarative vs. legacy)"
        );
    }
}

/// Sanity floor beneath the parity diff: proves the corpus actually exercises
/// the features it claims to (a parity test that diverged-and-both-sides-
/// stayed-empty would still pass `assert_eq!`).
#[test]
fn go_declarative_conventions_actually_extract_the_corpus_features() {
    let r = parse_declarative();

    let has = |label: &str, name: &str| r.nodes.iter().any(|n| n.label == label && n.name == name);
    assert!(has("Function", "NewWidget"));
    assert!(has("Function", "helper"));
    assert!(has("Method", "Area"), "pointer-receiver method");
    assert!(has("Method", "Name"), "value-receiver method");
    assert!(has("Struct", "Widget"));
    assert!(has("Struct", "gadget"));
    assert!(has("Trait", "Shaper"), "interface_type_kind -> Trait");
    assert!(has("Field", "Width"));
    assert!(has("Constant", "MaxWidgets"));
    assert!(
        has("Constant", "counter"),
        "grouped var block, multi-name DFS"
    );
    assert!(has("Import", "fmt"));
    assert!(has("Import", "strings"));
    assert!(has("Import", "sync"));

    let widget = r
        .nodes
        .iter()
        .find(|n| n.label == "Struct" && n.name == "Widget")
        .expect("Widget struct must be extracted");
    assert_eq!(widget.visibility, "public", "exported name -> public");
    let gadget = r
        .nodes
        .iter()
        .find(|n| n.label == "Struct" && n.name == "gadget")
        .expect("gadget struct must be extracted");
    assert_eq!(gadget.visibility, "package", "unexported name -> package");

    let area = r
        .nodes
        .iter()
        .find(|n| n.label == "Method" && n.name == "Area")
        .expect("Area method must be extracted");
    assert!(
        area.qualified_name.contains("::Widget::Area#"),
        "pointer receiver must strip `*` and scope under Widget, got {}",
        area.qualified_name
    );

    // The non-identifier callee `(fp)()` must be dropped by BOTH paths
    // identically (proven again, directly, beyond the set-equality check
    // above) — a bare `fp()` call on the same variable must still be kept.
    let calls: Vec<&str> = r
        .refs
        .iter()
        .filter(|e| e.kind == "Calls" && e.from_qualified_name.contains("dropped#"))
        .map(|e| e.to_qualified_name.as_str())
        .collect();
    assert_eq!(
        calls,
        vec!["fp"],
        "only the identifier-callee `fp()` may survive in `dropped`, got {calls:?}"
    );
}
