// doc_claims::counts: "N tests", "N proof harnesses", "an enum of N variants".
//
// Test and proof counts. The graph holds every `#[test]` / `#[kani::proof]`
// function its parser saw (`Function.entry_kind`, issue #354): a LOWER bound,
// since macro output is not parsed. So a claim above the graph's count is
// `not_verifiable`, never `contradicted`. A claim is `contradicted` only when
// the declarations the harness certainly compiles and runs (the compiled
// floor) already outnumber it, and `supported` only when the floor reaches
// the claim (for "exactly N", every declaration is in the floor and there are
// N): a declaration the build may drop supports nothing. A declaration enters
// the floor when:
//   - its file is reached by a target the harness runs (`File.target_context`):
//     `cargo test` runs the tests of lib, bin and test targets, and only builds
//     examples and benches, whose `test` setting defaults to false; `cargo kani`
//     verifies the harnesses of the package's lib and bin targets.
//     source: The Cargo Book, "Cargo Targets" (the `test` field) and
//     "cargo test" (target selection); Kani reference, "Usage" (`cargo kani`).
//   - no `#[cfg]` other than the harness's own option (`test`, `kani`) reaches
//     it, read from the source (`parser::rust_item_gates`), nor a `cfg_attr`;
//   - it is not nested in another function (such a `#[test]` is never run,
//     rustc lint `unnameable_test_items`).
// Anything the floor cannot decide leaves it, which only ever makes a
// contradiction harder to reach.
//
// Enum variants. A parsed enum's variants are all in its declaration, so the
// count is complete once the source agrees with the graph; a variant under its
// own `#[cfg]` makes the count depend on the build, and only a claim outside
// [ungated, declared] is then contradicted. Only Rust enums are read from the
// source; another language's count can only be `supported`.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::repo_files::RepoFiles;
use super::symbols::{absent, lookup, Candidate};
use super::{graph_evidence, repo_evidence, Claim, Outcome, Verdict};
use crate::graph_store::{cypher_str, GraphStore};
use crate::parser::rust_item_gates::{enum_shapes, function_gates, FunctionGate};

/// What a count of harness entry points counts, and what compiles them.
pub(super) struct Harness {
    entry_kind: &'static str,
    /// The `cfg` option the harness sets (`cargo test`: `test`; Kani: `kani`).
    option: &'static str,
    /// The `File.target_context` values whose targets the harness runs.
    contexts: &'static [&'static str],
}

pub(super) const TESTS: Harness = Harness {
    entry_kind: "test",
    option: "test",
    contexts: &["production", "test"],
};

pub(super) const PROOFS: Harness = Harness {
    entry_kind: "proof",
    option: "kani",
    contexts: &["production"],
};

/// The value a count claim states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    Exact(u64),
    AtLeast(u64),
}

fn parse_expected(raw: &str) -> Option<Expected> {
    let raw = raw.trim();
    match raw.strip_prefix(">=") {
        Some(n) => n.trim().parse().ok().map(Expected::AtLeast),
        None => raw.parse().ok().map(Expected::Exact),
    }
}

fn bad_expected(raw: &str) -> Outcome {
    Outcome::not_verifiable(
        format!(
            "expected_not_a_count: '{raw}'; write \"N\" for exactly N or \">=N\" for at least N"
        ),
        Vec::new(),
    )
}

/// The id prefix a count scope names: the whole graph for `""` or `"."`, a
/// directory for a path ending in `/`, else one file.
fn scope_prefix(subject: &str) -> Option<String> {
    let s = subject.trim().trim_start_matches("./");
    match s {
        "" | "." => None,
        dir if dir.ends_with('/') => Some(dir.to_string()),
        file => Some(format!("{file}::")),
    }
}

pub(super) fn entry_count(
    store: &GraphStore,
    files: &mut RepoFiles,
    claim: &Claim,
    harness: Harness,
) -> Outcome {
    let Some(expected) = parse_expected(&claim.expected) else {
        return bad_expected(&claim.expected);
    };
    let scope = scope_prefix(&claim.subject)
        .map(|p| format!(" AND f.id STARTS WITH {}", cypher_str(&p)))
        .unwrap_or_default();
    let query = format!(
        "MATCH (f:Function) WHERE f.entry_kind = {}{scope} RETURN f.id, coalesce(f.start_line, 0) \
         ORDER BY f.id",
        cypher_str(harness.entry_kind)
    );
    let rows = match store.execute_query(&query) {
        Ok(qr) => qr.rows,
        Err(e) => return Outcome::not_verifiable(format!("graph_query_failed: {e}"), Vec::new()),
    };
    let declared = rows.len() as u64;
    let (floor, excluded) = compiled_floor(store, files, &rows, &harness);
    let ids: Vec<&String> = rows.iter().map(|r| &r[0]).collect();
    let evidence = vec![
        graph_evidence(&query, json!({ "declared": declared, "ids": ids })),
        json!({
            "source": "graph+repository",
            "compiled_floor": floor,
            "left_out_of_floor": excluded,
            "epistemic": "lower-bound: the graph misses declarations its parser cannot see",
        }),
    ];
    count_verdict(expected, declared, floor, evidence)
}

/// The verdict on a harness count: contradicted by the floor, supported only
/// when the floor reaches the claim, else why the graph cannot decide.
fn count_verdict(expected: Expected, declared: u64, floor: u64, evidence: Vec<Value>) -> Outcome {
    let range = format!("declared {declared}, compiled floor {floor}");
    match expected {
        Expected::Exact(n) if floor > n => Outcome::new(
            Verdict::Contradicted,
            Some(format!("floor_exceeds_claim: {range}, claimed {n}")),
            evidence,
        ),
        Expected::Exact(n) if floor == n && declared == n => {
            Outcome::new(Verdict::Supported, None, evidence)
        }
        Expected::AtLeast(n) if floor >= n => Outcome::new(Verdict::Supported, None, evidence),
        Expected::Exact(n) | Expected::AtLeast(n) if n > declared => Outcome::not_verifiable(
            format!("lower_bound: {range}, claimed {n}; more may exist than the graph holds"),
            evidence,
        ),
        Expected::Exact(n) | Expected::AtLeast(n) => Outcome::not_verifiable(
            format!("build_dependent: {range}, claimed {n}; it depends on what the build compiles"),
            evidence,
        ),
    }
}

/// The count of `rows` the harness certainly compiles and runs, and why each
/// other row was left out.
fn compiled_floor(
    store: &GraphStore,
    files: &mut RepoFiles,
    rows: &[Vec<String>],
    harness: &Harness,
) -> (u64, BTreeMap<String, u64>) {
    let mut by_file: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for row in rows {
        let file = row[0].split("::").next().unwrap_or("");
        by_file
            .entry(file)
            .or_default()
            .push(row[1].parse().unwrap_or(0));
    }
    let mut floor = 0;
    let mut excluded: BTreeMap<String, u64> = BTreeMap::new();
    for (file, lines) in by_file {
        let verdicts: Vec<Option<String>> = match file_gates(store, files, file, harness) {
            Err(why) => lines.iter().map(|_| Some(why.clone())).collect(),
            Ok(gates) => lines
                .iter()
                .map(|line| function_verdict(&gates, *line, harness))
                .collect(),
        };
        for verdict in verdicts {
            match verdict {
                None => floor += 1,
                Some(why) => *excluded.entry(why).or_insert(0) += 1,
            }
        }
    }
    (floor, excluded)
}

/// The functions of `file` when the whole file is compiled by the harness,
/// else why it is not.
fn file_gates(
    store: &GraphStore,
    files: &mut RepoFiles,
    file: &str,
    harness: &Harness,
) -> Result<Vec<FunctionGate>, String> {
    if !file.ends_with(".rs") {
        return Err("not_rust_source".into());
    }
    for column in ["target_context", "cfg_gate"] {
        if !store.node_column_exists("File", column).unwrap_or(false) {
            return Err(format!("graph_without_file_{column}"));
        }
    }
    let query = format!(
        "MATCH (f:File) WHERE f.id = {} RETURN f.target_context, f.cfg_gate",
        cypher_str(file)
    );
    let row = store
        .execute_query(&query)
        .ok()
        .and_then(|qr| qr.rows.into_iter().next())
        .ok_or_else(|| "file_not_in_graph".to_string())?;
    if !harness.contexts.contains(&row[0].as_str()) {
        let context = if row[0].is_empty() {
            "none"
        } else {
            row[0].as_str()
        };
        return Err(format!("target_not_run_by_harness({context})"));
    }
    if !row[1].is_empty() && row[1] != harness.option {
        return Err(format!("file_gated({})", row[1]));
    }
    let source = files
        .read(file)
        .map_err(|e| format!("source_unreadable({})", e.0))?;
    function_gates(source).map_err(|_| "source_parse_failed".to_string())
}

/// `None` when the function at `line` is in the floor, else why not.
fn function_verdict(gates: &[FunctionGate], line: u64, harness: &Harness) -> Option<String> {
    let Some(gate) = gates.iter().find(|g| g.line == line) else {
        return Some("not_at_graph_line".into());
    };
    if gate.nested {
        return Some("nested_function".into());
    }
    if gate.cfg_attr {
        return Some("cfg_attr".into());
    }
    if !gate.gate.is_empty() && gate.gate != harness.option {
        return Some(format!("gated({})", gate.gate));
    }
    None
}

pub(super) fn variant_count(store: &GraphStore, files: &mut RepoFiles, claim: &Claim) -> Outcome {
    let Some(expected) = parse_expected(&claim.expected) else {
        return bad_expected(&claim.expected);
    };
    let (enum_node, mut evidence) = match single_enum(store, &claim.subject) {
        Ok(found) => found,
        Err(outcome) => return outcome,
    };
    let query = format!(
        "MATCH (e:Enum)-[:HasVariant_Enum_Variant]->(v:Variant) WHERE e.id = {} RETURN v.name \
         ORDER BY v.name",
        cypher_str(&enum_node.id)
    );
    let names: Vec<String> = match store.execute_query(&query) {
        Ok(qr) => qr.rows.into_iter().map(|r| r[0].clone()).collect(),
        Err(e) => return Outcome::not_verifiable(format!("graph_query_failed: {e}"), evidence),
    };
    let declared = names.len() as u64;
    evidence.push(graph_evidence(&query, json!({ "variants": names })));
    if enum_node.language != "rust" {
        return unread_language_verdict(expected, declared, &enum_node.language, evidence);
    }
    match rust_gated_variants(files, &enum_node.id, enum_node.start_line, declared) {
        Ok((gated, location)) => {
            evidence.push(repo_evidence(
                &location,
                json!({ "variants": declared, "with_their_own_cfg": gated }),
            ));
            variant_verdict(expected, declared, gated, claim.expected.trim(), evidence)
        }
        Err(why) => Outcome::not_verifiable(why, evidence),
    }
}

/// The one enum `subject` names, with the lookup's evidence, or the outcome
/// that ends the claim (empty subject, absent, ambiguous).
fn single_enum(store: &GraphStore, subject: &str) -> Result<(Candidate, Vec<Value>), Outcome> {
    if subject.trim().is_empty() {
        return Err(Outcome::not_verifiable("subject_empty", Vec::new()));
    }
    let found = lookup(store, subject, &["Enum"])
        .map_err(|e| Outcome::not_verifiable(format!("graph_query_failed: {e}"), Vec::new()))?;
    let evidence = found.evidence.clone();
    match found.candidates.as_slice() {
        [] => Err(absent(store, subject, evidence)),
        [only] => Ok((only.clone(), evidence)),
        _ => Err(Outcome::not_verifiable(
            format!(
                "ambiguous_subject: {} enums match ({} distinct); name one, or a #[cfg] twin, by \
                 its id",
                found.candidates.len(),
                found.distinct_items().len()
            ),
            evidence,
        )),
    }
}

/// A language whose enums are not read from source: the graph's count can
/// support a claim, never contradict one.
fn unread_language_verdict(
    expected: Expected,
    declared: u64,
    language: &str,
    evidence: Vec<Value>,
) -> Outcome {
    match expected {
        Expected::Exact(n) if n == declared => Outcome::new(Verdict::Supported, None, evidence),
        Expected::AtLeast(n) if n <= declared => Outcome::new(Verdict::Supported, None, evidence),
        _ => Outcome::not_verifiable(
            format!(
                "variant_completeness_not_established: '{language}' enums are not read from \
                 source; the graph holds {declared}"
            ),
            evidence,
        ),
    }
}

/// A Rust enum with `gated` of its `declared` variants under their own `#[cfg]`:
/// every build has between `declared - gated` and `declared` of them.
fn variant_verdict(
    expected: Expected,
    declared: u64,
    gated: u64,
    claimed: &str,
    evidence: Vec<Value>,
) -> Outcome {
    let ungated = declared - gated;
    let range = format!("{ungated} ungated of {declared} declared");
    let (low, high, certain) = match expected {
        Expected::Exact(n) => (n, n, gated == 0),
        Expected::AtLeast(n) => (n, u64::MAX, n <= ungated),
    };
    if high < ungated || low > declared {
        Outcome::new(
            Verdict::Contradicted,
            Some(format!("variant_count_differs: {range}, claimed {claimed}")),
            evidence,
        )
    } else if certain {
        Outcome::new(Verdict::Supported, None, evidence)
    } else {
        Outcome::not_verifiable(
            format!("build_dependent: {range}, claimed {claimed}"),
            evidence,
        )
    }
}

/// How many variants of the Rust enum at `line` of the enum's file carry their
/// own `#[cfg]`, once the source agrees with the graph on how many there are.
fn rust_gated_variants(
    files: &mut RepoFiles,
    enum_id: &str,
    line: u64,
    declared: u64,
) -> Result<(u64, String), String> {
    let file = enum_id.split("::").next().unwrap_or("");
    let location = format!("{file}:{line}");
    let source = files
        .read(file)
        .map_err(|e| format!("source_unreadable: {}: {}", file, e.0))?;
    let shapes = enum_shapes(source).map_err(|e| format!("source_parse_failed: {e}"))?;
    let shape = shapes
        .into_iter()
        .find(|s| s.line == line)
        .ok_or_else(|| format!("source_changed_since_indexing: no enum at {location}"))?;
    if shape.variants as u64 != declared {
        return Err(format!(
            "graph_and_source_disagree: the source declares {} variants at {location}, the graph \
             holds {declared}",
            shape.variants
        ));
    }
    Ok((shape.gated_variants as u64, location))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_counts_parse_exact_and_at_least_only() {
        assert_eq!(parse_expected(" 26 "), Some(Expected::Exact(26)));
        assert_eq!(parse_expected(">=8"), Some(Expected::AtLeast(8)));
        assert_eq!(parse_expected(">= 8"), Some(Expected::AtLeast(8)));
        assert_eq!(parse_expected("eight"), None);
        assert_eq!(parse_expected("-1"), None);
        assert_eq!(parse_expected(""), None);
    }

    #[test]
    fn a_scope_is_everything_a_directory_or_one_file() {
        assert_eq!(scope_prefix(""), None);
        assert_eq!(scope_prefix("."), None);
        assert_eq!(scope_prefix("tests/"), Some("tests/".into()));
        assert_eq!(scope_prefix("./src/lib.rs"), Some("src/lib.rs::".into()));
    }

    fn gate(line: u64, gate: &str) -> FunctionGate {
        FunctionGate {
            line,
            gate: gate.into(),
            cfg_attr: false,
            nested: false,
        }
    }

    #[test]
    fn only_the_harness_option_may_gate_a_floor_function() {
        let gates = vec![
            gate(1, ""),
            gate(2, "test"),
            gate(3, "unix"),
            gate(4, "kani"),
        ];
        assert_eq!(function_verdict(&gates, 1, &TESTS), None);
        assert_eq!(function_verdict(&gates, 2, &TESTS), None);
        assert_eq!(
            function_verdict(&gates, 3, &TESTS),
            Some("gated(unix)".into())
        );
        assert_eq!(
            function_verdict(&gates, 4, &TESTS),
            Some("gated(kani)".into())
        );
        assert_eq!(function_verdict(&gates, 4, &PROOFS), None);
        assert_eq!(
            function_verdict(&gates, 9, &TESTS),
            Some("not_at_graph_line".into())
        );
    }

    #[test]
    fn nested_and_cfg_attr_functions_leave_the_floor() {
        let mut nested = gate(1, "");
        nested.nested = true;
        let mut attr = gate(2, "");
        attr.cfg_attr = true;
        let gates = vec![nested, attr];
        assert_eq!(
            function_verdict(&gates, 1, &TESTS),
            Some("nested_function".into())
        );
        assert_eq!(function_verdict(&gates, 2, &TESTS), Some("cfg_attr".into()));
    }

    #[test]
    fn a_bad_expected_value_is_not_verifiable_with_the_syntax() {
        let o = bad_expected("many");
        assert_eq!(o.verdict, Verdict::NotVerifiable);
        assert!(o.reason.expect("reason").contains(">=N"));
    }
}
