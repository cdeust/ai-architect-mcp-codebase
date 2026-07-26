// parser::spec::python_parity_tests — pins the Python migration to the
// table-driven walkers at EXACT parity with the hand-written walker it replaced
// (ADR-0055 phase 2, §5 step 3).
//
// Before the old walker was deleted, a throwaway corpus-wide equivalence check
// (over all 41 `graph_accuracy` Python fixtures plus edge-case inline sources)
// asserted the spec walker reproduced `parse_python_file`'s output node-for-node
// and ref-for-ref. That scaffold is gone with the old walker; this committed
// test is the durable ground-truth pin. The expected records below ARE the
// hand-written walker's exact output on the corpus (full 7-tuple per node —
// label, name, qn, start/end line, visibility, ordered properties — and full
// ref triples), so per-EdgeKind F1(new-vs-groundtruth) = 1.0 == F1(old).
//
// It parses through the crate's public `parse_file`, so it also covers the
// Python dispatch arm. The end-to-end corpus parity (Nodes/Defines/Calls/
// Imports F1 = 1.000 across all 41 fixtures, resolver included) is held by the
// unchanged `tests/graph_accuracy.rs` gate, which now routes Python through
// these walkers.

use std::collections::BTreeSet;

use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

// One Python file exercising every concern the spec walker handles for Python:
//   - imports: `from __future__`, dotted (`os.path`), aliased (`numpy as np`),
//     from-import (`typing`), aliased from-import (`List as L`), glob
//     (`from collections.abc import *`) — all normalized `.`→`::` and emitted
//     as file-local `Defines` edges.
//   - module constants: `UPPER_SNAKE` typed (`MAX_SIZE: int`) and untyped
//     (`_PRIVATE`); a lowercase assignment (`logger = …`) that is NOT a constant
//     (the §12 negative case — see `python_lowercase_module_assignment_is_not_constant`).
//   - a plain function, and a `@decorator`-ed `async def` (decorator CSV + async).
//   - a class with two bases (identifier `Base` + attribute `pkg.Mixin`) → `Struct`
//     with `bases` CSV and two `Extends` refs (`.`→`::`).
//   - methods: a dunder (`__init__`, public), a private (`_speak`), and a
//     `@property`/`@setter` overload pair whose identical QN is disambiguated by
//     the collision dedup (`kind` and `kind@33`).
//   - call sites: bare (`format`), attribute (`client.get`), and a call inside a
//     method body (`helper`) — each a `Defines` edge with a byte-span QN.
const CORPUS: &str = r#"from __future__ import annotations
import os.path
import numpy as np
from typing import Any, List as L
from collections.abc import *

MAX_SIZE: int = 100
_PRIVATE = "hidden"
logger = get_logger()


def greet(name):
    return format(name)


@decorator
async def fetch(url):
    return await client.get(url)


class Animal(Base, pkg.Mixin):
    def __init__(self, name):
        self.name = name

    def _speak(self):
        helper()

    @property
    def kind(self):
        return self._kind

    @kind.setter
    def kind(self, v):
        self._kind = v
"#;

const PATH: &str = "zoo/animal.py";

fn node_record(n: &ExtractedNode) -> String {
    // Full 7-tuple with EXACT property order (not sorted) — order is part of
    // the output contract the spec walker must reproduce.
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
        "CallSite|client.get|zoo/animal.py::fetch::call@18:17#291-306|18|18||[(\"callee_name\", \"client.get\"), (\"caller_qn\", \"zoo/animal.py::fetch\")]",
        "CallSite|format|zoo/animal.py::greet::call@13:11#226-238|13|13||[(\"callee_name\", \"format\"), (\"caller_qn\", \"zoo/animal.py::greet\")]",
        "CallSite|helper|zoo/animal.py::Animal::_speak::call@26:8#426-434|26|26||[(\"callee_name\", \"helper\"), (\"caller_qn\", \"zoo/animal.py::Animal::_speak\")]",
        "Constant|MAX_SIZE|zoo/animal.py::MAX_SIZE|7|7||[(\"type_annotation\", \"int\")]",
        "Constant|_PRIVATE|zoo/animal.py::_PRIVATE|8|8||[(\"type_annotation\", \"\")]",
        "Function|fetch|zoo/animal.py::fetch|17|18||[(\"is_async\", \"true\"), (\"decorators\", \"decorator\")]",
        "Function|greet|zoo/animal.py::greet|12|13||[(\"is_async\", \"false\")]",
        "Import|L|zoo/animal.py::L|4|4||[(\"path\", \"typing::List\"), (\"alias\", \"L\"), (\"is_glob\", \"false\")]",
        "Import|__future__::annotations|zoo/animal.py::__future__::annotations|1|1||[(\"path\", \"__future__::annotations\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|collections::abc::*|zoo/animal.py::collections::abc::*|5|5||[(\"path\", \"collections::abc\"), (\"alias\", \"\"), (\"is_glob\", \"true\")]",
        "Import|np|zoo/animal.py::np|3|3||[(\"path\", \"numpy\"), (\"alias\", \"np\"), (\"is_glob\", \"false\")]",
        "Import|os::path|zoo/animal.py::os::path|2|2||[(\"path\", \"os::path\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Import|typing::Any|zoo/animal.py::typing::Any|4|4||[(\"path\", \"typing::Any\"), (\"alias\", \"\"), (\"is_glob\", \"false\")]",
        "Method|__init__|zoo/animal.py::Animal::__init__|22|23||[(\"is_async\", \"false\"), (\"receiver_type\", \"zoo/animal.py::Animal\")]",
        "Method|_speak|zoo/animal.py::Animal::_speak|25|26|private|[(\"is_async\", \"false\"), (\"receiver_type\", \"zoo/animal.py::Animal\")]",
        "Method|kind|zoo/animal.py::Animal::kind@33|33|34||[(\"is_async\", \"false\"), (\"decorators\", \"kind.setter\"), (\"receiver_type\", \"zoo/animal.py::Animal\")]",
        "Method|kind|zoo/animal.py::Animal::kind|29|30||[(\"is_async\", \"false\"), (\"decorators\", \"property\"), (\"receiver_type\", \"zoo/animal.py::Animal\")]",
        "Struct|Animal|zoo/animal.py::Animal|21|34||[(\"bases\", \"Base,pkg.Mixin\")]",
    ]
}

fn expected_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Defines", "zoo/animal.py", "zoo/animal.py::Animal"),
        ("Defines", "zoo/animal.py", "zoo/animal.py::L"),
        ("Defines", "zoo/animal.py", "zoo/animal.py::MAX_SIZE"),
        ("Defines", "zoo/animal.py", "zoo/animal.py::_PRIVATE"),
        (
            "Defines",
            "zoo/animal.py",
            "zoo/animal.py::__future__::annotations",
        ),
        (
            "Defines",
            "zoo/animal.py",
            "zoo/animal.py::collections::abc::*",
        ),
        ("Defines", "zoo/animal.py", "zoo/animal.py::fetch"),
        ("Defines", "zoo/animal.py", "zoo/animal.py::greet"),
        ("Defines", "zoo/animal.py", "zoo/animal.py::np"),
        ("Defines", "zoo/animal.py", "zoo/animal.py::os::path"),
        ("Defines", "zoo/animal.py", "zoo/animal.py::typing::Any"),
        (
            "Defines",
            "zoo/animal.py::Animal::_speak",
            "zoo/animal.py::Animal::_speak::call@26:8#426-434",
        ),
        (
            "Defines",
            "zoo/animal.py::fetch",
            "zoo/animal.py::fetch::call@18:17#291-306",
        ),
        (
            "Defines",
            "zoo/animal.py::greet",
            "zoo/animal.py::greet::call@13:11#226-238",
        ),
        ("Extends", "zoo/animal.py::Animal", "Base"),
        ("Extends", "zoo/animal.py::Animal", "pkg::Mixin"),
        (
            "HasMethod",
            "zoo/animal.py::Animal",
            "zoo/animal.py::Animal::__init__",
        ),
        (
            "HasMethod",
            "zoo/animal.py::Animal",
            "zoo/animal.py::Animal::_speak",
        ),
        (
            "HasMethod",
            "zoo/animal.py::Animal",
            "zoo/animal.py::Animal::kind",
        ),
        (
            "HasMethod",
            "zoo/animal.py::Animal",
            "zoo/animal.py::Animal::kind@33",
        ),
    ]
}

fn parse() -> ParseResult {
    parse_file(CORPUS, PATH, Language::Python).expect("python parse must not hard-fail")
}

#[test]
fn python_spec_output_is_exact_parity() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean Python must report 0 parse errors");

    let obs: BTreeSet<String> = r.nodes.iter().map(node_record).collect();
    let exp: BTreeSet<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        obs, exp,
        "Python node set (full record) diverged from the hand-written walker's ground truth"
    );

    let obs_refs: BTreeSet<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp_refs: BTreeSet<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    assert_eq!(
        obs_refs, exp_refs,
        "Python ref set diverged from the hand-written walker's ground truth"
    );
}

#[test]
fn python_per_edge_kind_f1_is_at_parity() {
    let r = parse();
    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();

    // Parser-emitted edge kinds for Python. `Imports`/`Calls` are resolver-
    // emitted (cross-file) and their F1 = 1.000 parity is held end-to-end by
    // `tests/graph_accuracy.rs`; here we pin the parser's own kinds.
    for kind in ["Defines", "HasMethod", "Extends"] {
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
            "Python {kind} F1 {f1:.3} is below parity 1.000"
        );
    }
}

/// Negative assertion: a lowercase module-level assignment (`logger = …`) is
/// NOT a constant. This is the §12 kill for the `walk_value_decl` guard's
/// `||` → `&&` mutant — under `&&`, `logger` would be emitted as a spurious
/// `Constant`; under `||` it is correctly skipped. Phase 1 could only document
/// this mutant as equivalent (Go's `is_constant_name` is constant-true);
/// Python's `UPPER_SNAKE` filter makes it observable and killable.
#[test]
fn python_lowercase_module_assignment_is_not_constant() {
    let r = parse();
    let has_logger_constant = r
        .nodes
        .iter()
        .any(|n| n.label == "Constant" && n.name == "logger");
    assert!(
        !has_logger_constant,
        "lowercase `logger` must NOT be extracted as a Constant (kills the \
         walk_value_decl ||→&& mutant)"
    );
    // And a module-level call (`get_logger()`) is NOT a call site — call sites
    // are extracted only inside function/method bodies.
    let has_module_callsite = r
        .nodes
        .iter()
        .any(|n| n.label == "CallSite" && n.name == "get_logger");
    assert!(
        !has_module_callsite,
        "module-level call `get_logger()` must NOT produce a CallSite"
    );
}

/// A `from __future__ import a, b as c` yields both a plain and an ALIASED
/// future import — the aliased one uses the alias as display name. Pins the
/// `aliased_import` arm of `imports_future` (§12: kills the "delete the
/// aliased-import match arm" mutant, which the main corpus's single unaliased
/// future import left alive).
#[test]
fn python_future_import_alias_is_extracted() {
    let src = "from __future__ import annotations, division as d\n";
    let r = parse_file(src, "f.py", Language::Python).expect("parse");
    let imports: Vec<(&str, &str)> = r
        .nodes
        .iter()
        .filter(|n| n.label == "Import")
        .map(|n| {
            let path = n
                .properties
                .iter()
                .find(|(k, _)| k == "path")
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            (n.name.as_str(), path)
        })
        .collect();
    assert!(
        imports.contains(&("__future__::annotations", "__future__::annotations")),
        "plain future import missing: {imports:?}"
    );
    assert!(
        imports.contains(&("d", "__future__::division")),
        "aliased future import `division as d` missing (display=`d`): {imports:?}"
    );
}

/// Module constants are extracted only at MODULE scope — an `UPPER_SNAKE`
/// assignment inside a class body (a class variable) or a function body is NOT
/// a module `Constant`. Pins the `enclosing_class.is_none()` gate in
/// `walk_defs` (§12: kills the `&&` → `||` mutant, under which a class-level
/// `TIMEOUT = 30` would wrongly surface as a `Constant`).
#[test]
fn python_class_level_upper_snake_is_not_a_module_constant() {
    let src =
        "TOP_LEVEL = 1\n\nclass Cfg:\n    TIMEOUT = 30\n    RETRIES = 5\n\n    def m(self):\n        LOCAL_MAX = 9\n        return LOCAL_MAX\n";
    let r = parse_file(src, "c.py", Language::Python).expect("parse");
    let constants: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.label == "Constant")
        .map(|n| n.name.as_str())
        .collect();
    // Only the genuine module-level constant is extracted.
    assert_eq!(
        constants,
        vec!["TOP_LEVEL"],
        "only module-scope UPPER_SNAKE is a Constant; class/function-body ones are not: {constants:?}"
    );
}

/// The `@property`/`@setter` overload pair shares `qual(scope, name)`, so the
/// collision dedup must give the second one a distinct primary key — both
/// `Method` nodes exist with distinct QNs (the resolver still finds either by
/// name). Kills any mutant that drops the dedup and collapses the pair.
#[test]
fn python_property_setter_pair_is_deduped() {
    let r = parse();
    let kind_qns: BTreeSet<&str> = r
        .nodes
        .iter()
        .filter(|n| n.label == "Method" && n.name == "kind")
        .map(|n| n.qualified_name.as_str())
        .collect();
    assert_eq!(
        kind_qns.len(),
        2,
        "the @property/@setter `kind` pair must yield two distinct method QNs, got {kind_qns:?}"
    );
    assert!(kind_qns.contains("zoo/animal.py::Animal::kind"));
    assert!(kind_qns.contains("zoo/animal.py::Animal::kind@33"));
}
