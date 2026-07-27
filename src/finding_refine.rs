//! Stage 1b — `refine_finding`: the orchestrator-refined prompt schema and
//! the handler logic. Extracted from `main.rs` per issue #151
//! (Fowler: Extract Class).

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

use crate::finding_extract::ExtractedFinding;
use crate::finding_index::*;
use crate::handler_util::*;

// source: stages/stage-1.md §4.2 / §9.2 — orchestrator's refined prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RefinedPrompt {
    pub(crate) text: String,
    pub(crate) role_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token_estimate: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AddedContext {
    pub(crate) kind: String,
    pub(crate) content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provenance: Option<String>,
}

// source: stages/stage-1.md §9.2 — refinement metadata from the orchestrator.
// `refined_at` is filled in by the Rust tool, not the agent: the JSON Schema
// (§9.2 line 402) lists only `added_context` + `orchestrator_version` with
// `additionalProperties: false`, and `#[serde(skip_deserializing)]` below
// enforces the same invariant at the Rust layer — any `refined_at` an agent
// sends is silently dropped before the tool fills its own timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RefinementMeta {
    pub(crate) added_context: Vec<AddedContext>,
    pub(crate) orchestrator_version: String,
    #[serde(default, skip_deserializing)]
    pub(crate) refined_at: Option<String>,
}

// source: stages/stage-1.md §4.2 — full refined artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RefinedArtifact {
    pub(crate) extracted: ExtractedFinding,
    pub(crate) refined_prompt: RefinedPrompt,
    pub(crate) refinement: RefinementMeta,
}
// ---------------------------------------------------------------------------
// Stage 1b — refine_finding
// ---------------------------------------------------------------------------

// Err.0 is the short machine-readable reason code used by the smoke test
// (e.g. "no_extraction"), Err.1 is the human-readable message.
pub(crate) struct RefineArgs {
    pub(crate) run_id: String,
    pub(crate) finding_id: String,
    pub(crate) output_dir: PathBuf,
    pub(crate) refined_prompt: RefinedPrompt,
    pub(crate) refinement_input: RefinementMeta,
}

pub(crate) fn run_refine_finding(arguments: &Value) -> Value {
    match do_refine_finding(arguments) {
        Ok(v) => v,
        Err((reason_code, reason_msg)) => json!({
            "stage": 1,
            "status": "error",
            "reason": reason_code,
            "message": reason_msg
        }),
    }
}

pub(crate) fn require_string_arg(args: &Map<String, Value>, key: &str) -> Result<String, StageErr> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| bad_request(format!("missing required field '{}' (spec §9.2)", key)))
}

pub(crate) fn parse_refine_args(arguments: &Value) -> Result<RefineArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §9.2)"))?;

    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;

    let refined_prompt_val = args
        .get("refined_prompt")
        .cloned()
        .ok_or_else(|| bad_request("missing required field 'refined_prompt' (spec §9.2)"))?;
    let refined_prompt: RefinedPrompt =
        serde_json::from_value(refined_prompt_val).map_err(|e| {
            bad_request(format!(
                "refined_prompt does not match schema (spec §9.2): {}",
                e
            ))
        })?;
    // spec §5.1.5 — non-empty refinement contract.
    if refined_prompt.text.is_empty() {
        return Err((
            "empty_prompt".to_string(),
            "refined_prompt.text must be non-empty (spec §5.1.5)".to_string(),
        ));
    }

    let refinement_val = args
        .get("refinement")
        .cloned()
        .ok_or_else(|| bad_request("missing required field 'refinement' (spec §9.2)"))?;
    let refinement_input: RefinementMeta = serde_json::from_value(refinement_val).map_err(|e| {
        bad_request(format!(
            "refinement does not match schema (spec §9.2): {}",
            e
        ))
    })?;

    Ok(RefineArgs {
        run_id,
        finding_id,
        output_dir,
        refined_prompt,
        refinement_input,
    })
}

// spec §9.2 precondition 1 — extraction must exist, parse cleanly.
pub(crate) fn load_existing_extracted(finding_dir: &Path) -> Result<ExtractedFinding, StageErr> {
    let extracted_path = finding_dir.join(EXTRACTED_FILE_NAME);
    if !extracted_path.exists() {
        return Err((
            "no_extraction".to_string(),
            format!(
                "no stage-1.extracted.json at {:?} — call extract_finding first (spec §9.2)",
                extracted_path
            ),
        ));
    }
    let extracted_raw = fs::read_to_string(&extracted_path)
        .map_err(|e| io_err(format!("read {:?}: {}", extracted_path, e)))?;
    serde_json::from_str(&extracted_raw).map_err(|e| {
        (
            "corrupt_extraction".to_string(),
            format!("parse {:?}: {}", extracted_path, e),
        )
    })
}

// spec §4.2 — pure construction of the refined artifact. Fills `refined_at`
// server-side per §9.2 (agent input is silently dropped by serde skip).
pub(crate) fn build_refined_artifact(
    extracted: ExtractedFinding,
    refined_prompt: RefinedPrompt,
    refinement_input: RefinementMeta,
    refined_at: String,
) -> RefinedArtifact {
    RefinedArtifact {
        extracted,
        refined_prompt,
        refinement: RefinementMeta {
            added_context: refinement_input.added_context,
            orchestrator_version: refinement_input.orchestrator_version,
            refined_at: Some(refined_at),
        },
    }
}

// spec §5.2.3 — atomic write of the refined artifact + §5.2 index upsert in
// MergeMode::Replace (refine_finding owns all four index fields).
pub(crate) fn persist_refine(
    output_dir: &Path,
    run_id: &str,
    finding_id: &str,
    finding_dir: &Path,
    artifact: &RefinedArtifact,
    refined_at: String,
) -> Result<(PathBuf, usize), StageErr> {
    let refined_path = finding_dir.join(REFINED_FILE_NAME);
    let bytes_written = write_json_atomic(&refined_path, artifact).map_err(io_err)?;
    let entry = IndexEntry {
        artifact_path: format!("{}/{}/{}", FINDINGS_DIR_NAME, finding_id, REFINED_FILE_NAME),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        orchestrator_version: Some(artifact.refinement.orchestrator_version.clone()),
        refined_at: Some(refined_at),
        ..IndexEntry::default()
    };
    // PreserveStage2: a re-refine must not clobber the stage-2 verified_*/
    // stage2_path fields an earlier finalize_verification wrote.
    upsert_index_entry(
        output_dir,
        run_id,
        finding_id,
        entry,
        MergeMode::PreserveStage2,
    )
    .map_err(io_err)?;
    Ok((refined_path, bytes_written))
}

pub(crate) fn do_refine_finding(arguments: &Value) -> Result<Value, StageErr> {
    let parsed = parse_refine_args(arguments)?;
    let finding_dir = parsed
        .output_dir
        .join(RUNS_DIR_NAME)
        .join(&parsed.run_id)
        .join(FINDINGS_DIR_NAME)
        .join(&parsed.finding_id);

    let extracted = load_existing_extracted(&finding_dir)?;
    let refined_at = format_iso8601_utc(now_unix_seconds_nanos().0);
    let orchestrator_version = parsed.refinement_input.orchestrator_version.clone();
    let artifact = build_refined_artifact(
        extracted,
        parsed.refined_prompt,
        parsed.refinement_input,
        refined_at.clone(),
    );
    let (refined_path, bytes_written) = persist_refine(
        &parsed.output_dir,
        &parsed.run_id,
        &parsed.finding_id,
        &finding_dir,
        &artifact,
        refined_at,
    )?;

    Ok(json!({
        "stage": 1,
        "status": "ok",
        "finding_id": parsed.finding_id,
        "artifact_path": refined_path.to_string_lossy(),
        "run_id": parsed.run_id,
        "bytes_written": bytes_written,
        "extractor_version": EXTRACTOR_VERSION,
        "orchestrator_version": orchestrator_version,
        "orchestrator_contract_version": ORCHESTRATOR_CONTRACT_VERSION
    }))
}
