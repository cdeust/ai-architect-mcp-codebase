// parser::spec::parity_tests — pins the Go migration to the table-driven
// walkers at EXACT parity with the hand-written walker they replaced
// (ADR-0055 phase 1, §5 step 3).
//
// Before the old walker was deleted, a temporary equivalence check asserted
// the spec walker reproduced `parse_go_file`'s output node-for-node and
// ref-for-ref on this corpus. That scaffold is gone with the old walker; this
// committed test is the durable ground-truth pin. The expected records below
// ARE the hand-written walker's output (full 7-tuple per node: label, name,
// qn, start/end line, visibility, properties — and full ref triples), so
// per-EdgeKind F1(new-vs-groundtruth) = 1.0 == F1(old-vs-groundtruth).
//
// It parses through the crate's public `parse_file`, so it also covers the
// Go dispatch arm, and prints the per-EdgeKind precision/recall/F1 the PR
// parity table quotes.

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

// One Go file exercising every concern the spec walker handles: grouped and
// single imports, const, var, a type alias, a struct (single- AND multi-name
// fields), an interface, a free function with calls, a receiver-scoped method
// with calls, and two trailing free functions. source: hand-authored; the
// expected records below are the exact output of the pre-migration walker.
//
// Deliberate edge cases that pin `call_callee` and the multi-name field loop
// (each kills a specific mutant the plainer corpus left alive):
//   - `Width, High int` — one field_declaration with TWO names; both must
//     surface (kills the "stop after first name" mutant in walk_fields).
//   - `_init()` — a `_`-prefixed callee, kept by the identifier-start rule
//     (kills the `== '_'` → `!= '_'` mutant in call_callee).
//   - `(handler)()` — a parenthesized (non-identifier) callee, which the
//     callee filter correctly DROPS: there is NO CallSite for it (negative
//     assertion; kills the `&&` → `||` mutant in call_callee).
const CORPUS: &str = r#"package app

import (
	"fmt"
	"net/http"
)

import "strings"

const MaxRetries = 3
var GlobalName = "svc"

type Status int

type Config struct {
	Name        string
	Port        int
	Width, High int
}

type Handler interface {
	Serve() error
}

func NewConfig(name string) *Config {
	c := &Config{Name: name}
	fmt.Println(name)
	return c
}

func (c *Config) Validate() error {
	strings.TrimSpace(c.Name)
	helper()
	return nil
}

func helper() {}

func setup() {
	_init()
	(handler)()
}
"#;

const PATH: &str = "app/main.go";

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
        "CallSite|Println|app/main.go::NewConfig#1::call@27:2#2|27|27|public|[(\"callee_name\", \"Println\"), (\"lsp_col\", \"1\")]",
        "CallSite|TrimSpace|app/main.go::Config::Validate#3::call@32:2#5|32|32|public|[(\"callee_name\", \"TrimSpace\"), (\"lsp_col\", \"1\")]",
        "CallSite|_init|app/main.go::setup#7::call@40:2#8|40|40|public|[(\"callee_name\", \"_init\"), (\"lsp_col\", \"1\")]",
        "CallSite|helper|app/main.go::Config::Validate#3::call@33:2#4|33|33|public|[(\"callee_name\", \"helper\"), (\"lsp_col\", \"1\")]",
        "Constant|GlobalName|app/main.go::GlobalName|11|11|public|[]",
        "Constant|MaxRetries|app/main.go::MaxRetries|10|10|public|[]",
        "Field|High|app/main.go::Config::High|18|18|public|[(\"type_annotation\", \"int\")]",
        "Field|Name|app/main.go::Config::Name|16|16|public|[(\"type_annotation\", \"string\")]",
        "Field|Port|app/main.go::Config::Port|17|17|public|[(\"type_annotation\", \"int\")]",
        "Field|Width|app/main.go::Config::Width|18|18|public|[(\"type_annotation\", \"int\")]",
        "Function|NewConfig|app/main.go::NewConfig#1|25|29|public|[(\"return_type\", \"*Config\"), (\"constructed_types\", \"Config\")]",
        "Function|helper|app/main.go::helper#6|37|37|package|[]",
        "Function|setup|app/main.go::setup#7|39|42|package|[]",
        "Import|fmt|app/main.go::import:fmt|4|4|public|[(\"path\", \"fmt\")]",
        "Import|http|app/main.go::import:net/http|5|5|public|[(\"path\", \"net/http\")]",
        "Import|strings|app/main.go::import:strings|8|8|public|[(\"path\", \"strings\")]",
        "Method|Validate|app/main.go::Config::Validate#3|31|35|public|[(\"receiver_type\", \"Config\"), (\"return_type\", \"error\")]",
        "Struct|Config|app/main.go::Config|15|19|public|[]",
        "Trait|Handler|app/main.go::Handler|21|23|public|[]",
        "TypeAlias|Status|app/main.go::Status|13|13|public|[]",
    ]
}

fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Calls", "app/main.go::Config::Validate#3", "TrimSpace"),
        ("Calls", "app/main.go::Config::Validate#3", "helper"),
        ("Calls", "app/main.go::NewConfig#1", "Println"),
        ("Calls", "app/main.go::setup#7", "_init"),
        ("Defines", "app/main.go", "app/main.go::Config"),
        ("Defines", "app/main.go", "app/main.go::GlobalName"),
        ("Defines", "app/main.go", "app/main.go::Handler"),
        ("Defines", "app/main.go", "app/main.go::MaxRetries"),
        ("Defines", "app/main.go", "app/main.go::NewConfig#1"),
        ("Defines", "app/main.go", "app/main.go::Status"),
        ("Defines", "app/main.go", "app/main.go::helper#6"),
        ("Defines", "app/main.go", "app/main.go::setup#7"),
        (
            "HasField",
            "app/main.go::Config",
            "app/main.go::Config::High",
        ),
        (
            "HasField",
            "app/main.go::Config",
            "app/main.go::Config::Name",
        ),
        (
            "HasField",
            "app/main.go::Config",
            "app/main.go::Config::Port",
        ),
        (
            "HasField",
            "app/main.go::Config",
            "app/main.go::Config::Width",
        ),
        (
            "HasMethod",
            "app/main.go::Config",
            "app/main.go::Config::Validate#3",
        ),
        ("Imports", "app/main.go", "fmt"),
        ("Imports", "app/main.go", "net/http"),
        ("Imports", "app/main.go", "strings"),
    ]
}

fn parse() -> ParseResult {
    parse_file(CORPUS, PATH, Language::Go).expect("go parse must not hard-fail")
}

#[test]
fn go_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean Go must report 0 parse errors");

    let obs: BTreeSet<String> = r.nodes.iter().map(node_record).collect();
    let exp: BTreeSet<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        obs, exp,
        "Go node set (full record) diverged from the hand-written walker's ground truth"
    );

    let obs_refs: BTreeSet<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp_refs: BTreeSet<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    assert_eq!(
        obs_refs, exp_refs,
        "Go ref set diverged from the hand-written walker's ground truth"
    );
}

#[test]
fn go_per_edge_kind_f1_is_at_parity() {
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
            "Go {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}
