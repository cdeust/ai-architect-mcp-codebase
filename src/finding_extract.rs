//! Stage 1a — `extract_finding`: the canonical `Finding` input schema, the
//! deterministic `ExtractedFinding` output schema, and the handler logic.
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::finding_index::*;
use crate::handler_util::*;

// ---------------------------------------------------------------------------
// Stage 1 schema types (one Rust type per spec schema)
// ---------------------------------------------------------------------------

// source: stages/stage-1.md §3.2 — canonical finding shape.
// additionalProperties: true → extra fields are captured in `extras`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Finding {
    pub(crate) id: String,
    pub(crate) title: String,
    // §5.1.1 preservation + §9.3 Q8: `None` must serialize as JSON `null`,
    // not absent. `default` is kept so input-absent still parses. Removing
    // `skip_serializing_if` makes `null` and `0.0` round-trip distinctly in
    // `stage-1.source.json`. (spec: stages/stage-1.md §5.1.1, §9.3 Q8)
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) source_url: Option<String>,
    pub(crate) relevance_category: String,
    #[serde(default)]
    pub(crate) relevance_score: Option<f64>,
    #[serde(default)]
    pub(crate) raw_data: Option<Value>,
    // Spec §3.2 declares additionalProperties: true and §5.1.1 (preservation)
    // requires round-tripping unknown fields. Kept as a flat map.
    // INVARIANT: adding a new explicit field to this struct requires verifying
    // it does NOT collide with any key that the input stream might put in
    // `extras`. On collision, serde flatten emits duplicate JSON keys — most
    // parsers silently keep the last, which violates §5.1.1 preservation.
    // Check the field name against recent `raw_data` shapes before adding.
    #[serde(flatten)]
    pub(crate) extras: BTreeMap<String, Value>,
}

// source: stages/stage-1.md §4.1 — deterministic extraction output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExtractedFinding {
    pub(crate) finding_id: String,
    pub(crate) title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_url: Option<String>,
    pub(crate) relevance_category: String,
    // Nullable per §9.3 Q8 — `null` and 0.0 must round-trip distinctly.
    pub(crate) relevance_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) raw_data: Option<Value>,
    pub(crate) extracted_at: String,
    pub(crate) extractor_version: String,
    pub(crate) source_form: String,
    pub(crate) source_path: Option<String>,
    // §5.1.1 preservation of unknown fields from the canonical finding.
    // INVARIANT: adding a new explicit field to this struct requires verifying
    // it does NOT collide with any key that the input stream might put in
    // `extras`. On collision, serde flatten emits duplicate JSON keys — most
    // parsers silently keep the last, which violates §5.1.1 preservation.
    // Check the field name against recent `raw_data` shapes before adding.
    #[serde(flatten)]
    pub(crate) extras: BTreeMap<String, Value>,
}
// ---------------------------------------------------------------------------
// Stage 1 — finding resolution (spec §3.3 form 1 + form 2)
// ---------------------------------------------------------------------------

/// (finding, source_form, source_path, source_bytes_verbatim).
/// `source_bytes_verbatim` is the canonical JSON bytes to write into
/// stage-1.source.json — after normalization to the §3.2 schema, per §4.4.
/// clippy::type_complexity.
pub(crate) type ResolvedFinding = (Finding, &'static str, Option<String>, Vec<u8>);

pub(crate) fn resolve_finding(finding_arg: &Value) -> Result<ResolvedFinding, String> {
    match finding_arg {
        Value::Object(_) => {
            let finding: Finding = serde_json::from_value(finding_arg.clone()).map_err(|e| {
                format!("inline finding does not match canonical schema §3.2: {}", e)
            })?;
            validate_required_finding_fields(&finding)?;
            let bytes = serde_json::to_vec_pretty(&finding)
                .map_err(|e| format!("serialize inline finding: {}", e))?;
            Ok((finding, "inline", None, bytes))
        }
        Value::String(path_str) => {
            let path = require_absolute(path_str, "finding")?;
            let lower = path_str.to_ascii_lowercase();
            if lower.ends_with(".md") {
                // spec §9.3 Q1 — .md input is deferred in v1.
                return Err(
                    ".md finding inputs are not supported in v1 (spec §9.3 Q1); \
                     convert to JSON first"
                        .to_string(),
                );
            }
            if !lower.ends_with(".json") {
                return Err(format!(
                    "finding path must end in .json (spec §3.3): got {:?}",
                    path_str
                ));
            }
            let raw = fs::read_to_string(&path)
                .map_err(|e| format!("read finding file {:?}: {}", path, e))?;
            let parsed: Value = serde_json::from_str(&raw)
                .map_err(|e| format!("parse finding file {:?}: {}", path, e))?;

            // §3.3 form 2: either the root matches §3.2, or the root is
            // {findings: [...]} with exactly one element.
            let finding_value = if let Some(arr) = parsed
                .as_object()
                .and_then(|m| m.get("findings"))
                .and_then(|v| v.as_array())
            {
                if arr.len() != 1 {
                    return Err(format!(
                        "finding file {:?} has findings[{}]: stage 1 processes one finding per call (spec §3.3)",
                        path,
                        arr.len()
                    ));
                }
                arr[0].clone()
            } else {
                parsed
            };

            let finding: Finding = serde_json::from_value(finding_value).map_err(|e| {
                format!(
                    "finding file {:?} does not match canonical schema §3.2: {}",
                    path, e
                )
            })?;
            validate_required_finding_fields(&finding)?;
            let bytes = serde_json::to_vec_pretty(&finding)
                .map_err(|e| format!("serialize finding: {}", e))?;
            Ok((
                finding,
                "json_file",
                Some(path.to_string_lossy().into_owned()),
                bytes,
            ))
        }
        _ => Err("finding must be an object or an absolute path string (spec §3.1)".to_string()),
    }
}

pub(crate) fn validate_required_finding_fields(f: &Finding) -> Result<(), String> {
    if f.id.trim().is_empty() {
        return Err("finding.id is required and must be non-empty (spec §3.2)".to_string());
    }
    if f.title.trim().is_empty() {
        return Err("finding.title is required and must be non-empty (spec §3.2)".to_string());
    }
    if f.relevance_category.trim().is_empty() {
        return Err(
            "finding.relevance_category is required and must be non-empty (spec §3.2)".to_string(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Stage 1a — extract_finding
// ---------------------------------------------------------------------------

pub(crate) struct ExtractArgs<'a> {
    pub(crate) finding_arg: &'a Value,
    pub(crate) output_dir: PathBuf,
    pub(crate) run_id: String,
}

pub(crate) fn run_extract_finding(arguments: &Value) -> Value {
    match do_extract_finding(arguments) {
        Ok(v) => v,
        Err(reason) => json!({
            "stage": 1,
            "status": "error",
            "reason": reason
        }),
    }
}

pub(crate) fn parse_extract_args(arguments: &Value) -> Result<ExtractArgs<'_>, String> {
    let args = arguments
        .as_object()
        .ok_or_else(|| "arguments must be an object (spec §3.1)".to_string())?;
    let finding_arg = args
        .get("finding")
        .ok_or_else(|| "missing required field 'finding' (spec §3.1)".to_string())?;
    let output_dir_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing required field 'output_dir' (spec §3.1)".to_string())?;
    let output_dir = require_absolute(output_dir_str, "output_dir")?;
    let run_id = match args.get("run_id") {
        Some(Value::String(s)) => {
            validate_safe_id("run_id", s)?;
            s.clone()
        }
        Some(Value::Null) | None => generate_run_id(),
        Some(other) => {
            return Err(format!(
                "run_id must be a string when provided (spec §3.1): got {}",
                other
            ));
        }
    };
    Ok(ExtractArgs {
        finding_arg,
        output_dir,
        run_id,
    })
}

// spec §4.1: pure construction of the canonical extracted artifact.
pub(crate) fn build_extracted_artifact(
    finding: &Finding,
    source_form: &'static str,
    source_path: Option<String>,
) -> ExtractedFinding {
    ExtractedFinding {
        finding_id: finding.id.clone(),
        title: finding.title.clone(),
        description: finding.description.clone(),
        source_url: finding.source_url.clone(),
        relevance_category: finding.relevance_category.clone(),
        relevance_score: finding.relevance_score,
        raw_data: finding.raw_data.clone(),
        extracted_at: format_iso8601_utc(now_unix_seconds_nanos().0),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        source_form: source_form.to_string(),
        source_path,
        extras: finding.extras.clone(),
    }
}

// spec §4.4 + §5.2.3 + §5.2 — mkdir, atomic-write source, atomic-write
// extracted, and upsert the preliminary index entry in MergeMode::PreserveRefined
// (so a prior refined artifact is not clobbered by a re-extract).
pub(crate) fn persist_extract(
    output_dir: &Path,
    run_id: &str,
    finding_dir: &Path,
    source_bytes: &[u8],
    extracted: &ExtractedFinding,
) -> Result<(PathBuf, usize), String> {
    fs::create_dir_all(finding_dir).map_err(|e| format!("mkdir {:?}: {}", finding_dir, e))?;
    atomic_write(&finding_dir.join(SOURCE_FILE_NAME), source_bytes)?;
    let extracted_path = finding_dir.join(EXTRACTED_FILE_NAME);
    let bytes_written = write_json_atomic(&extracted_path, extracted)?;
    let entry = IndexEntry {
        artifact_path: format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, extracted.finding_id, EXTRACTED_FILE_NAME
        ),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        ..IndexEntry::default()
    };
    // PreserveDownstream: a re-extract must not clobber fields that stage 1b
    // or stage 2 later wrote on top (refined_*, verified_*, stage2_path).
    upsert_index_entry(
        output_dir,
        run_id,
        &extracted.finding_id,
        entry,
        MergeMode::PreserveDownstream,
    )?;
    Ok((extracted_path, bytes_written))
}

pub(crate) fn do_extract_finding(arguments: &Value) -> Result<Value, String> {
    let parsed = parse_extract_args(arguments)?;
    let (finding, source_form, source_path, source_bytes) = resolve_finding(parsed.finding_arg)?;
    // spec §5.1.4, §9.3 Q4 — hard-fail on unsafe finding_id BEFORE touching disk.
    validate_safe_id("finding_id", &finding.id)?;

    let finding_dir = parsed
        .output_dir
        .join(RUNS_DIR_NAME)
        .join(&parsed.run_id)
        .join(FINDINGS_DIR_NAME)
        .join(&finding.id);
    let extracted = build_extracted_artifact(&finding, source_form, source_path);
    let (extracted_path, bytes_written) = persist_extract(
        &parsed.output_dir,
        &parsed.run_id,
        &finding_dir,
        &source_bytes,
        &extracted,
    )?;

    // spec §4.3 — MCP receipt. artifact_path here is the extracted artifact;
    // refine_finding updates it to point at the refined artifact later.
    Ok(json!({
        "stage": 1,
        "status": "ok",
        "finding_id": finding.id,
        "artifact_path": extracted_path.to_string_lossy(),
        "run_id": parsed.run_id,
        "bytes_written": bytes_written,
        "extractor_version": EXTRACTOR_VERSION
    }))
}
