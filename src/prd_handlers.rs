//! Stage 4/6/8/9 — `prepare_prd_input`, `verify_semantic_diff`,
//! `validate_prd_against_graph`, and `check_security_gates` handler logic.
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use crate::graph_cache;
use crate::handler_util::*;
use crate::prd_input;
use crate::prd_validator;
use crate::security_gates;
use crate::semantic_diff;

pub(crate) fn run_prepare_prd_input(arguments: &Value) -> Value {
    match do_prepare_prd_input(arguments) {
        Ok(v) => v,
        Err(msg) => stage4_error_response(&msg),
    }
}

pub(crate) fn stage4_error_response(msg: &str) -> Value {
    let reason = if msg.starts_with("stage_2_not_verified") {
        "stage_2_not_verified"
    } else if msg.starts_with("stage_1_refined_missing")
        || msg.starts_with("stage_1_refined_unreadable")
        || msg.starts_with("stage_1_refined_corrupt")
    {
        "stage_1_refined_missing"
    } else {
        "prepare_prd_input_failed"
    };
    json!({
        "stage": 4,
        "status": "error",
        "reason": reason,
        "message": msg,
    })
}

pub(crate) fn do_prepare_prd_input(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    // run_id defaults to "adhoc" — required only as a path segment. In feature
    // mode the caller often has no pipeline run.
    let run_id = args
        .get("run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("adhoc")
        .to_string();
    validate_safe_id("run_id", &run_id)?;
    // finding_id is OPTIONAL now: present → finding mode (verified stage-2
    // gate); absent → feature mode (requires feature_description).
    let finding_id = match args.get("finding_id").and_then(|v| v.as_str()) {
        Some(fid) => {
            validate_safe_id("finding_id", fid)?;
            Some(fid.to_string())
        }
        None => None,
    };
    let feature_description = args
        .get("feature_description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if finding_id.is_none() && feature_description.is_none() {
        return Err("missing input: provide 'finding_id' (finding mode) or \
             'feature_description' (feature mode)"
            .to_string());
    }
    let output_dir_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'output_dir'")?;
    let output_dir = require_absolute(output_dir_str, "output_dir")?;
    let graph_path_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = require_absolute(graph_path_str, "graph_path")?;
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_path_str}"));
    }

    let prepared_at = format_iso8601_utc(now_unix_seconds_nanos().0);
    let outcome = prd_input::prepare(
        &prd_input::PrdInputArgs {
            run_id: run_id.clone(),
            finding_id: finding_id.clone(),
            feature_description: feature_description.clone(),
            output_dir,
            graph_path,
        },
        prepared_at.clone(),
    )?;

    Ok(json!({
        "stage": 4,
        "status": "ok",
        "tool": "prepare_prd_input",
        "mode": if finding_id.is_some() { "finding" } else { "feature" },
        "run_id": run_id,
        "finding_id": finding_id,
        "artifact_path": outcome.artifact_path.to_string_lossy(),
        "prepared_at": prepared_at,
        "matched_symbol_count": outcome.matched_symbol_count,
        // Lexical-only hits, never counted as verified grounding (issue #14).
        "candidate_symbol_count": outcome.candidate_symbol_count,
        "impacted_community_count": outcome.impacted_community_count,
        "impacted_process_count": outcome.impacted_process_count,
        // Grounding returned inline so the PRD generator can inject it without
        // a second read of artifact_path.
        "prd_context": outcome.prd_context,
        "preparer_version": prd_input::PREPARER_VERSION,
    }))
}

// ---------------------------------------------------------------------------
// Stage 9 — verify_semantic_diff (diff two graphs; flag regressions)
// ---------------------------------------------------------------------------

pub(crate) fn run_verify_semantic_diff(arguments: &Value) -> Value {
    match do_verify_semantic_diff(arguments) {
        Ok(v) => v,
        Err(msg) => {
            let reason = if msg.contains("before_graph_path_missing") {
                "before_graph_path_missing"
            } else if msg.contains("after_graph_path_missing") {
                "after_graph_path_missing"
            } else {
                "verify_semantic_diff_failed"
            };
            json!({
                "stage": 9,
                "status": "error",
                "reason": reason,
                "message": msg,
            })
        }
    }
}

pub(crate) fn do_verify_semantic_diff(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let before_str = args
        .get("before_graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'before_graph_path'")?;
    let after_str = args
        .get("after_graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'after_graph_path'")?;
    let before_graph_path = require_absolute(before_str, "before_graph_path")?;
    let after_graph_path = require_absolute(after_str, "after_graph_path")?;
    let report_path = args
        .get("report_path")
        .and_then(|v| v.as_str())
        .map(|s| require_absolute(s, "report_path"))
        .transpose()?;

    let verified_at = format_iso8601_utc(now_unix_seconds_nanos().0);
    let outcome = semantic_diff::diff(
        &semantic_diff::SemanticDiffArgs {
            before_graph_path: before_graph_path.clone(),
            after_graph_path: after_graph_path.clone(),
        },
        verified_at.clone(),
    )?;

    let written = match &report_path {
        Some(p) => {
            semantic_diff::write_report(p, &outcome.report)?;
            Some(p.to_string_lossy().to_string())
        }
        None => None,
    };

    Ok(json!({
        "stage": 9,
        "status": "ok",
        "tool": "verify_semantic_diff",
        "verified_at": verified_at,
        "summary": {
            "nodes_added": outcome.summary.nodes_added,
            "nodes_removed": outcome.summary.nodes_removed,
            "edges_added": outcome.summary.edges_added,
            "edges_removed": outcome.summary.edges_removed,
            "dangling_references": outcome.summary.dangling_references,
            "new_unresolved_delta": outcome.summary.new_unresolved_delta,
            "new_cycles": outcome.summary.new_cycles,
        },
        "regression_score": outcome.regression_score,
        "verdict": outcome.verdict,
        "report": outcome.report,
        "report_path": written,
    }))
}

// ---------------------------------------------------------------------------
// Stage 6 — validate_prd_against_graph
// ---------------------------------------------------------------------------

pub(crate) fn run_validate_prd_against_graph(arguments: &Value) -> Value {
    match do_validate_prd_against_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 6,
            "status": "error",
            "reason": "validate_prd_against_graph_failed",
            "message": msg,
        }),
    }
}

pub(crate) fn do_validate_prd_against_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let prd_path_str = args
        .get("prd_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'prd_path'")?;
    let prd_path = require_absolute(prd_path_str, "prd_path")?;
    let graph_path_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = require_absolute(graph_path_str, "graph_path")?;
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_path_str}"));
    }
    let affected_opt = args
        .get("affected_symbols_path")
        .and_then(|v| v.as_str())
        .map(|s| require_absolute(s, "affected_symbols_path"))
        .transpose()?;

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(&graph_path)?;
    let report = prd_validator::validate_prd(&store, &prd_path, affected_opt.as_deref())?;
    let validated_at = format_iso8601_utc(now_unix_seconds_nanos().0);

    let (run_id_opt, finding_id_opt, output_dir_opt) = extract_optional_ids(args)?;
    let artifact_path = maybe_write_validation(
        &report,
        &prd_path,
        &graph_path,
        &validated_at,
        &run_id_opt,
        &finding_id_opt,
        &output_dir_opt,
    )?;

    let json_report = prd_validator::report_to_json(
        &report,
        run_id_opt.as_deref().unwrap_or(""),
        finding_id_opt.as_deref().unwrap_or(""),
        &prd_path,
        &graph_path,
        &validated_at,
    );
    Ok(json!({
        "stage": 6,
        "status": "ok",
        "tool": "validate_prd_against_graph",
        "validated_at": validated_at,
        "validation_status": report.validation_status,
        "extraction_mode": report.extraction_mode,
        "contract_missing": report.contract_missing,
        "summary": {
            "claimed_symbols": report.summary.claimed_symbols,
            "resolved_symbols": report.summary.resolved_symbols,
            "hallucinated_symbols": report.summary.hallucinated_symbols,
            "unverifiable_symbols": report.summary.unverifiable_symbols,
            "communities_spanned": report.summary.communities_spanned,
            "processes_impacted": report.summary.processes_impacted,
        },
        "artifact_path": artifact_path.map(|p| p.to_string_lossy().to_string()),
        "report": json_report,
    }))
}

/// (run_id, feature_id, graph_path), all optional/independently-present.
/// clippy::type_complexity.
pub(crate) type OptionalRunIds = (Option<String>, Option<String>, Option<PathBuf>);

pub(crate) fn extract_optional_ids(args: &Map<String, Value>) -> Result<OptionalRunIds, String> {
    let run_id = args
        .get("run_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    if let Some(ref r) = run_id {
        validate_safe_id("run_id", r)?;
    }
    let finding_id = args
        .get("finding_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    if let Some(ref f) = finding_id {
        validate_safe_id("finding_id", f)?;
    }
    let output_dir = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .map(|s| require_absolute(s, "output_dir"))
        .transpose()?;
    Ok((run_id, finding_id, output_dir))
}

pub(crate) fn maybe_write_validation(
    report: &prd_validator::ValidationReport,
    prd_path: &Path,
    graph_path: &Path,
    validated_at: &str,
    run_id: &Option<String>,
    finding_id: &Option<String>,
    output_dir: &Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let (r, f, o) = match (run_id, finding_id, output_dir) {
        (Some(r), Some(f), Some(o)) => (r, f, o),
        _ => return Ok(None),
    };
    let value = prd_validator::report_to_json(report, r, f, prd_path, graph_path, validated_at);
    let dest = o
        .join("runs")
        .join(r)
        .join("findings")
        .join(f)
        .join(prd_validator::VALIDATION_FILE);
    let written = prd_validator::write_validation(&dest, &value)?;
    Ok(Some(written))
}

// ---------------------------------------------------------------------------
// Stage 8 — check_security_gates
// ---------------------------------------------------------------------------

pub(crate) fn run_check_security_gates(arguments: &Value) -> Value {
    match do_check_security_gates(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 8,
            "status": "error",
            "reason": "check_security_gates_failed",
            "message": msg,
        }),
    }
}

pub(crate) fn do_check_security_gates(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_path_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = require_absolute(graph_path_str, "graph_path")?;
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_path_str}"));
    }
    let changed_symbols: Vec<String> = args
        .get("changed_symbols")
        .and_then(|v| v.as_array())
        .ok_or("missing required field 'changed_symbols' (array of strings)")?
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(&graph_path)?;
    let report = security_gates::check_gates(&store, &changed_symbols)?;
    let checked_at = format_iso8601_utc(now_unix_seconds_nanos().0);

    let (run_id_opt, finding_id_opt, output_dir_opt) = extract_optional_ids(args)?;
    let artifact_path = maybe_write_security(
        &report,
        &graph_path,
        &changed_symbols,
        &checked_at,
        &run_id_opt,
        &finding_id_opt,
        &output_dir_opt,
    )?;

    let json_report = security_gates::report_to_json(
        &report,
        run_id_opt.as_deref().unwrap_or(""),
        finding_id_opt.as_deref().unwrap_or(""),
        &graph_path,
        &changed_symbols,
        &checked_at,
    );
    Ok(json!({
        "stage": 8,
        "status": "ok",
        "tool": "check_security_gates",
        "checked_at": checked_at,
        "gates_passed": report.gates_passed,
        "summary": {
            "changed_symbols": report.summary.changed_symbols,
            "critical_count": report.summary.critical_count,
            "warning_count": report.summary.warning_count,
            "info_count": report.summary.info_count,
        },
        "artifact_path": artifact_path.map(|p| p.to_string_lossy().to_string()),
        "report": json_report,
    }))
}

pub(crate) fn maybe_write_security(
    report: &security_gates::SecurityReport,
    graph_path: &Path,
    changed_symbols: &[String],
    checked_at: &str,
    run_id: &Option<String>,
    finding_id: &Option<String>,
    output_dir: &Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let (r, f, o) = match (run_id, finding_id, output_dir) {
        (Some(r), Some(f), Some(o)) => (r, f, o),
        _ => return Ok(None),
    };
    let value =
        security_gates::report_to_json(report, r, f, graph_path, changed_symbols, checked_at);
    let dest = o
        .join("runs")
        .join(r)
        .join("findings")
        .join(f)
        .join(security_gates::SECURITY_FILE);
    let written = security_gates::write_security(&dest, &value)?;
    Ok(Some(written))
}
