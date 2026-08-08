// runner_tests.rs — unit tests for runner.rs's scoring extraction.
//
// Split out of runner.rs (issue #218: runner.rs was 852 lines, over the
// 500-line file cap in coding-standards.md §4.1) via #[path] inclusion so
// the production dispatch/extraction code and its tests are physically
// separate files while staying in the same module namespace.
// #[allow(clippy::...)] not needed — this file is included as `mod tests`
// from runner.rs's tail, identical to an inline #[cfg(test)] mod.

use super::*;
use serde_json::json;

/// Regression for issue #214: a q4 ("callers") label must be scored
/// against `called_by` (+ `used_by` for instantiation sites), never the
/// union of every relationship kind — the union counted `calls` as
/// false positives and collapsed precision on every real q4/q5/q6
/// label (root-cause verified live on
/// `indexing_handlers.rs::do_index_codebase`: 1 true caller vs a
/// 21-item union before this fix).
#[test]
fn test_get_context_callers_ignores_unrelated_calls_edges() {
    let payload = json!({
        "relationships": {
            "called_by": [{"qualified_name": "mod.rs::run_wrapper"}],
            "calls": [
                {"qualified_name": "handler_util.rs::require_absolute"},
                {"qualified_name": "handler_util.rs::parse_bool_arg"}
            ],
            "implements": [], "implemented_by": [],
            "imports": [], "imported_by": [],
            "uses": [], "used_by": []
        }
    });
    let expected = json!({"callers": ["mod.rs::run_wrapper"]});
    let actual = extract_actual_set("get_context", &payload, &expected);
    assert_eq!(actual, vec!["mod.rs::run_wrapper".to_string()]);
    assert_eq!(score_f1(&["mod.rs::run_wrapper".to_string()], &actual), 1.0);
}

/// Regression for issue #214: a class instantiation (`new Foo(...)`) is
/// graphed as `used_by`, not `called_by` — a q4 ("callers") label on a
/// class/struct must still find its instantiation site. Verified live
/// on `logger.ts::ConsoleLogger` (`calls`/`called_by` both empty,
/// `used_by` holds `app.ts::App::constructor`).
#[test]
fn test_get_context_callers_includes_instantiation_via_used_by() {
    let payload = json!({
        "relationships": {
            "called_by": [], "calls": [],
            "implements": [], "implemented_by": [],
            "imports": [], "imported_by": [],
            "uses": [],
            "used_by": [{"qualified_name": "app.ts::App::constructor"}]
        }
    });
    let expected = json!({"callers": ["app.ts::App::constructor"]});
    let actual = extract_actual_set("get_context", &payload, &expected);
    assert_eq!(actual, vec!["app.ts::App::constructor".to_string()]);
}

/// Regression for issue #214: `get_impact`'s real schema has no
/// `affected` field (the pre-fix scorer always read None from it,
/// silently scoring q7 as a vacuous empty-vs-empty match). The fixed
/// extractor must read the real `callers`/`importers`/`users`/
/// `implementors` lists.
#[test]
fn test_get_impact_extracts_real_dependents_not_affected() {
    let payload = json!({
        "callers": [{"qualified_name": "main.rs::handle_tool_call"}],
        "importers": [{"qualified_name": "app.ts"}],
        "users": [{"qualified_name": "app.ts::App::logger"}],
        "implementors": [{"qualified_name": "logger.ts::ConsoleLogger"}],
        "affected": ["should-be-ignored"]
    });
    let expected = json!({"affected": []});
    let mut actual = extract_actual_set("get_impact", &payload, &expected);
    actual.sort();
    assert_eq!(
        actual,
        vec![
            "app.ts".to_string(),
            "app.ts::App::logger".to_string(),
            "logger.ts::ConsoleLogger".to_string(),
            "main.rs::handle_tool_call".to_string(),
        ]
    );
    assert!(!actual.contains(&"should-be-ignored".to_string()));
}

/// Regression for B3: the Q13 scorer extracts axis names from
/// `report.findings[].axis` (the real validate_prd_against_graph
/// response shape) and scores precision/recall vs the expected set.
#[test]
fn test_q13_scorer_reads_findings_axis() {
    // Case 1: prd_02_hallucinated — one symbol_hallucination finding.
    let payload_hallucinated = json!({
        "status": "ok",
        "validation_status": "has_errors",
        "report": {
            "findings": [
                {"axis": "symbol_hallucination", "severity": "error",
                 "message": "claim 'Foo' does not resolve"}
            ]
        }
    });
    let expected = json!({ "truly_present": ["symbol_hallucination"] });
    let score = score_prmean_from_payload(&payload_hallucinated, &expected).expect("score");
    assert!(
        score >= 0.8,
        "prd_02_hallucinated expected >=0.8, got {score}"
    );

    // Case 2: prd_03_community_violation — one community_consistency finding.
    let payload_community = json!({
        "status": "ok",
        "report": {
            "findings": [
                {"axis": "community_consistency", "severity": "warn",
                 "message": "scope spans unexpected communities"}
            ]
        }
    });
    let expected = json!({ "truly_present": ["community_consistency"] });
    let score = score_prmean_from_payload(&payload_community, &expected).expect("score");
    assert!(
        score >= 0.8,
        "prd_03_community_violation expected >=0.8, got {score}"
    );

    // Case 3: prd_01_valid — empty expected, empty actual → 1.0.
    let payload_valid = json!({
        "status": "ok",
        "report": { "findings": [] }
    });
    let expected = json!({ "truly_present": [] });
    let score = score_prmean_from_payload(&payload_valid, &expected).expect("score");
    assert!(
        (score - 1.0).abs() < 1e-9,
        "empty expected + empty actual should be 1.0, got {score}"
    );

    // Case 4: axis set is deduplicated + unrelated noise is tolerated
    // only as precision loss (not a scoring crash).
    let payload_mixed = json!({
        "report": {
            "findings": [
                {"axis": "symbol_hallucination"},
                {"axis": "symbol_hallucination"},
                {"axis": "process_impact"}
            ]
        }
    });
    let expected = json!({ "truly_present": ["symbol_hallucination"] });
    let score = score_prmean_from_payload(&payload_mixed, &expected).expect("score");
    // Recall = 1.0, precision = 0.5 → mean = 0.75.
    assert!(
        (score - 0.75).abs() < 1e-6,
        "mixed-axis payload: expected 0.75, got {score}"
    );
}
