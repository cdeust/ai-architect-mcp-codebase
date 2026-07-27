// parser::spec::cpp_parity_tests — the exact-set and per-edge-kind F1 assertions
// over the C++ ground truth in the sibling `cpp_ground_truth` module (which
// documents, row by row, why each entry is what it is).
//
// The suite parses through the crate's public `parse_file`, so it also covers the
// C++ dispatch arm. The exact-set assertion is deliberately a SET EQUALITY, not a
// containment check: it fails on a spurious node as loudly as on a missing one,
// which is what makes it the mutation oracle for the whole C++ walker.
//
// Per-clause pins for issues #123 and #124 live in `cpp_extraction_tests`; this
// module answers "is the whole output exactly right", that one answers "which
// clause does each row satisfy".

use std::collections::BTreeSet;

use super::cpp_ground_truth::{expected_node_records, expected_refs, CORPUS, PATH};
use crate::parser::{parse_file, ExtractedNode, ExtractedRef, Language, ParseResult};

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
    parse_file(CORPUS, PATH, Language::Cpp).expect("cpp parse must not hard-fail")
}

#[test]
fn cpp_spec_output_is_exact_set() {
    let r = parse();
    assert_eq!(r.parse_errors, 0, "clean C++ must report 0 parse errors");

    let obs: BTreeSet<String> = r.nodes.iter().map(node_record).collect();
    let exp: BTreeSet<String> = expected_node_records()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        obs, exp,
        "C++ node set (full record) diverged from the documented ground truth"
    );

    let obs_refs: BTreeSet<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp_refs: BTreeSet<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();
    assert_eq!(
        obs_refs, exp_refs,
        "C++ ref set diverged from the documented ground truth"
    );
}

/// Every emitted QN is unique. C++ relies on `#{seq}` instead of the walker's
/// collision dedup, so a `seq` that failed to advance once per definition would
/// silently collapse two symbols onto one primary key — and the set assertion
/// above, being a SET, cannot see a duplicate.
#[test]
fn cpp_qualified_names_are_unique() {
    let r = parse();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for n in &r.nodes {
        assert!(
            seen.insert(n.qualified_name.as_str()),
            "duplicate qualified name {:?} — a `seq` was reused",
            n.qualified_name
        );
    }
}

#[test]
fn cpp_per_edge_kind_f1_is_at_parity() {
    let r = parse();
    let obs_refs: Vec<(String, String, String)> = r.refs.iter().map(ref_triple).collect();
    let exp: Vec<(String, String, String)> = expected_refs()
        .into_iter()
        .map(|(k, f, t)| (k.to_string(), f.to_string(), t.to_string()))
        .collect();

    for kind in [
        "Defines",
        "HasMethod",
        "HasField",
        "Extends",
        "Imports",
        "Calls",
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
        // A kind with no expected AND no observed edges would score a vacuous
        // F1 = 1.000, so the corpus must actually exercise every kind listed.
        assert!(
            !exp_k.is_empty(),
            "edge kind {kind} has no expected edges: its F1 would be vacuous"
        );
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
            "C++ {kind} F1 {f1:.3} is below 1.000"
        );
    }
}
