//! The on-disk `index.json` model shared by the extract and refine finding
//! stages: `Index`/`IndexEntry` plus the read/merge/write logic. Extracted
//! from `main.rs` per issue #151 (Fowler: Extract Class).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::handler_util::*;

// source: stages/stage-1.md §5.2 + stages/stage-2.md §5.4 — index entry.
// Stage-2 fields are optional because a finding may be extracted/refined but
// not yet verified. `skip_serializing_if = Option::is_none` is CORRECT here
// (different from `Finding.relevance_score` which needed null/absent
// distinction per §9.3 Q8) — the index is server-owned metadata with no
// null/absent distinction issue: absent means the stage has not run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct IndexEntry {
    pub(crate) artifact_path: String,
    pub(crate) extractor_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) orchestrator_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) refined_at: Option<String>,
    // --- stage-2 (stages/stage-2.md §5.4) ---
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stage2_path: Option<String>,
}

// source: stages/stage-1.md §5.2 — index.json shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Index {
    pub(crate) run_id: String,
    pub(crate) started_at: String,
    pub(crate) last_updated_at: String,
    pub(crate) findings: BTreeMap<String, IndexEntry>,
}
// ---------------------------------------------------------------------------
// Stage 1 — index.json read/merge/write (spec §5.2)
// ---------------------------------------------------------------------------

pub(crate) fn read_index(path: &Path) -> Result<Option<Index>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("read index {:?}: {}", path, e))?;
    let idx: Index =
        serde_json::from_str(&raw).map_err(|e| format!("parse index {:?}: {}", path, e))?;
    Ok(Some(idx))
}

// Strategy for merging a new index entry with an existing one.
//
// Preservation discipline (spec §5.1.2 idempotency + §5.2.1 unique appearance
// + stages/stage-2.md §5.4): each stage owns its own fields, but a re-run of
// an earlier stage must NOT clobber downstream fields the later stage wrote.
//
// MergeMode answers the question "which fields of `entry` are authoritative?"
//
// Layer diagram (each arrow = "preserves when re-run"):
//
//     extract_finding  ─┐
//     refine_finding   ─┼──▶ IndexEntry
//     finalize_verification ──┘
//
// | Mode                   | Called by             | Preserves on top of `entry`                                   |
// |------------------------|-----------------------|---------------------------------------------------------------|
// | `PreserveDownstream`   | extract_finding       | refined_* (if set) AND verified_*/stage2_path (if set)         |
// | `PreserveStage2`       | refine_finding        | verified_*/stage2_path (if set) — stage-2 survives re-refine   |
// | `PreserveRefinedOnly`  | finalize_verification | refined_* (if set) — stage-2 fields come from `entry`          |
// | `Replace`              | (reserved, unused)    | nothing — `entry` is written wholesale                         |
//
// Written explicitly here because the stage-1 code-reviewer flagged MergeMode
// preservation as the single most hazardous area of the index code.
pub(crate) enum MergeMode {
    PreserveDownstream,
    PreserveStage2,
    PreserveRefinedOnly,
    #[allow(dead_code)]
    Replace,
}

pub(crate) fn upsert_index_entry(
    output_dir: &Path,
    run_id: &str,
    finding_id: &str,
    entry: IndexEntry,
    mode: MergeMode,
) -> Result<(), String> {
    let index_path = output_dir
        .join(RUNS_DIR_NAME)
        .join(run_id)
        .join(INDEX_FILE_NAME);
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let mut idx = match read_index(&index_path)? {
        Some(existing) => existing,
        None => Index {
            run_id: run_id.to_string(),
            started_at: now.clone(),
            last_updated_at: now.clone(),
            findings: BTreeMap::new(),
        },
    };
    idx.last_updated_at = now;

    let merged = merge_index_entry(idx.findings.get(finding_id), entry, mode);
    idx.findings.insert(finding_id.to_string(), merged);
    write_json_atomic(&index_path, &idx)?;
    Ok(())
}

// Pure function: given the existing entry (if any), the new entry, and the
// merge mode, return the entry that should be written. Split out of
// `upsert_index_entry` to keep that function under the 40-LOC bar and to
// make the preservation discipline testable in isolation.
pub(crate) fn merge_index_entry(
    existing: Option<&IndexEntry>,
    entry: IndexEntry,
    mode: MergeMode,
) -> IndexEntry {
    match mode {
        MergeMode::Replace => entry,
        MergeMode::PreserveDownstream => match existing {
            Some(e) => merge_preserve_downstream(e, entry),
            None => entry,
        },
        MergeMode::PreserveStage2 => match existing {
            Some(e) => merge_preserve_stage2(e, entry),
            None => entry,
        },
        MergeMode::PreserveRefinedOnly => match existing {
            Some(e) => merge_preserve_refined_only(e, entry),
            None => entry,
        },
    }
}

// extract_finding re-run: keep refined_* (if set) AND verified_*/stage2_path.
pub(crate) fn merge_preserve_downstream(existing: &IndexEntry, entry: IndexEntry) -> IndexEntry {
    IndexEntry {
        artifact_path: if existing.refined_at.is_some() {
            existing.artifact_path.clone()
        } else {
            entry.artifact_path
        },
        extractor_version: entry.extractor_version,
        orchestrator_version: existing.orchestrator_version.clone(),
        refined_at: existing.refined_at.clone(),
        verified_at: existing.verified_at.clone(),
        verified: existing.verified,
        stage2_path: existing.stage2_path.clone(),
    }
}

// refine_finding re-run: keep verified_*/stage2_path from the existing entry.
pub(crate) fn merge_preserve_stage2(existing: &IndexEntry, entry: IndexEntry) -> IndexEntry {
    IndexEntry {
        artifact_path: entry.artifact_path,
        extractor_version: entry.extractor_version,
        orchestrator_version: entry.orchestrator_version,
        refined_at: entry.refined_at,
        verified_at: existing.verified_at.clone(),
        verified: existing.verified,
        stage2_path: existing.stage2_path.clone(),
    }
}

// finalize_verification: keep refined_* from existing; stage-2 fields come
// from `entry`. artifact_path is kept as the refined path (stage 1b wrote it).
pub(crate) fn merge_preserve_refined_only(existing: &IndexEntry, entry: IndexEntry) -> IndexEntry {
    IndexEntry {
        artifact_path: existing.artifact_path.clone(),
        extractor_version: existing.extractor_version.clone(),
        orchestrator_version: existing.orchestrator_version.clone(),
        refined_at: existing.refined_at.clone(),
        verified_at: entry.verified_at,
        verified: entry.verified,
        stage2_path: entry.stage2_path,
    }
}
