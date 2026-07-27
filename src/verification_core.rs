//! Stage 2 — verification session schema types, the session state machine,
//! and session file I/O shared by start/append/finalize/abort.
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::handler_util::*;

// ---------------------------------------------------------------------------
// Stage 2 — schema types (spec §12.3)
// ---------------------------------------------------------------------------

// source: stages/stage-2.md §3 + §12.3 — the five legal session states.
// `deny_unknown_fields` at the field level not needed — the enum string
// values are exhaustive and unknown strings are rejected by serde by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionState {
    Open,
    WaitingForUser,
    WaitingForAgent,
    Finalized,
    Aborted,
}

impl SessionState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SessionState::Open => "open",
            SessionState::WaitingForUser => "waiting_for_user",
            SessionState::WaitingForAgent => "waiting_for_agent",
            SessionState::Finalized => "finalized",
            SessionState::Aborted => "aborted",
        }
    }
}

// source: stages/stage-2.md §12.3 transcript item schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnKind {
    AgentQuestion,
    UserAnswer,
}

// source: stages/stage-2.md §12.3 — transcript element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionTurn {
    pub(crate) seq: usize,
    pub(crate) kind: TurnKind,
    pub(crate) timestamp: String,
    pub(crate) content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) meta: Option<Value>,
}

// source: stages/stage-2.md §12.3 — single session file schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionFile {
    pub(crate) run_id: String,
    pub(crate) finding_id: String,
    pub(crate) state: SessionState,
    pub(crate) turn_count: usize,
    pub(crate) started_at: String,
    pub(crate) updated_at: String,
    pub(crate) schema_ok: bool,
    pub(crate) verifier_version: String,
    pub(crate) transcript: Vec<SessionTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) aborted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) abort_reason: Option<String>,
}

// source: stages/stage-2.md §5.3 — verified receipt sub-object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VerifiedKind {
    pub(crate) schema_ok: bool,
    pub(crate) completeness_ok: bool,
    pub(crate) user_acknowledged: bool,
}

// source: stages/stage-2.md §5.3 — verified receipt.
// `completeness_checklist` records which sub-items passed (per §5.3 + §9
// item 6: knowing WHICH item failed is load-bearing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VerifiedArtifact {
    pub(crate) run_id: String,
    pub(crate) finding_id: String,
    pub(crate) verified: bool,
    pub(crate) verified_kind: VerifiedKind,
    pub(crate) finalized_at: String,
    pub(crate) stage1_refined_path: String,
    pub(crate) session_path: String,
    pub(crate) transcript_digest: String,
    pub(crate) digest_algorithm: String,
    pub(crate) transcript_bytes_at_finalize: usize,
    pub(crate) turn_count: usize,
    pub(crate) verifier_version: String,
    pub(crate) completeness_checklist: BTreeMap<String, bool>,
}

// ---------------------------------------------------------------------------
// Stage 2 — state machine (spec §3 + §12.2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub(crate) enum TransitionEvent {
    Start,
    AgentQuestion,
    UserAnswer,
    Finalize,
    Abort,
}

// Pure function. Dispatches to per-event handlers so each branch stays tiny.
// Table is the §12.2 override of the §3 state machine; see the match arms in
// `transition_append`, `transition_finalize`, `transition_abort`, and
// `transition_start`.
pub(crate) fn can_transition(
    from: SessionState,
    event: TransitionEvent,
) -> Result<SessionState, StageErr> {
    // Terminal states reject everything except Aborted+Start (handled below).
    if matches!(from, SessionState::Finalized) {
        return Err(stage2_err(
            "already_finalized",
            "session is finalized; further writes are rejected (spec §8.4)",
        ));
    }
    if matches!(from, SessionState::Aborted) && !matches!(event, TransitionEvent::Start) {
        return Err(stage2_err(
            "already_aborted",
            "session is aborted; only start_verification may restart it (spec §12.1)",
        ));
    }
    match event {
        TransitionEvent::Start => transition_start(from),
        TransitionEvent::AgentQuestion => transition_append(from, TurnKind::AgentQuestion),
        TransitionEvent::UserAnswer => transition_append(from, TurnKind::UserAnswer),
        TransitionEvent::Finalize => transition_finalize(from),
        TransitionEvent::Abort => transition_abort(from),
    }
}

// Spec §7.1 + §12.1: start is legal only on a missing or aborted session.
// The "missing session" case never reaches this function (checked in the
// caller). This handles the "existing session" case.
pub(crate) fn transition_start(from: SessionState) -> Result<SessionState, StageErr> {
    match from {
        SessionState::Aborted => Ok(SessionState::Open),
        _ => Err(stage2_err(
            "illegal_transition",
            "start_verification requires no session or an aborted session (spec §7.1, §12.1)",
        )),
    }
}

// Spec §3 alternation invariant + §12.3. `Open → agent_question` is the
// first-turn case.
pub(crate) fn transition_append(
    from: SessionState,
    kind: TurnKind,
) -> Result<SessionState, StageErr> {
    match (from, kind) {
        (SessionState::Open, TurnKind::AgentQuestion) => Ok(SessionState::WaitingForUser),
        (SessionState::WaitingForUser, TurnKind::UserAnswer) => Ok(SessionState::WaitingForAgent),
        (SessionState::WaitingForAgent, TurnKind::AgentQuestion) => Ok(SessionState::WaitingForUser),
        _ => Err(stage2_err(
            "alternation_violation",
            "two consecutive turns of the same kind are illegal (spec §3 alternation invariant, §12.2)",
        )),
    }
}

// Spec §12.2: finalize is illegal from Open and WaitingForUser; legal from
// WaitingForAgent only.
pub(crate) fn transition_finalize(from: SessionState) -> Result<SessionState, StageErr> {
    match from {
        SessionState::WaitingForAgent => Ok(SessionState::Finalized),
        SessionState::Open => Err(stage2_err(
            "no_clarification_round",
            "finalize requires ≥1 agent_question AND ≥1 user_answer before it (spec §12.2)",
        )),
        SessionState::WaitingForUser => Err(stage2_err(
            "unanswered_question",
            "cannot finalize while an agent_question is awaiting a user_answer (spec §7.3, §12.2)",
        )),
        _ => Err(stage2_err(
            "illegal_transition",
            "finalize rejected (spec §12.2)",
        )),
    }
}

// Spec §12.1: abort is legal from any non-terminal state. Terminal-state
// rejection already happened at the top of can_transition.
pub(crate) fn transition_abort(_from: SessionState) -> Result<SessionState, StageErr> {
    Ok(SessionState::Aborted)
}

// Helper to build a StageErr with a stage-2 reason code (spec "error codes"
// list in the implementation brief).
pub(crate) fn stage2_err(code: &str, msg: &str) -> StageErr {
    (code.to_string(), msg.to_string())
}

// ---------------------------------------------------------------------------
// Stage 2 — session I/O (spec §12.3 atomicity-by-construction)
// ---------------------------------------------------------------------------

pub(crate) fn session_path_for(output_dir: &Path, run_id: &str, finding_id: &str) -> PathBuf {
    output_dir
        .join(RUNS_DIR_NAME)
        .join(run_id)
        .join(FINDINGS_DIR_NAME)
        .join(finding_id)
        .join(SESSION_FILE_NAME)
}

pub(crate) fn finding_dir_for(output_dir: &Path, run_id: &str, finding_id: &str) -> PathBuf {
    output_dir
        .join(RUNS_DIR_NAME)
        .join(run_id)
        .join(FINDINGS_DIR_NAME)
        .join(finding_id)
}

pub(crate) fn read_session(path: &Path) -> Result<Option<SessionFile>, StageErr> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).map_err(|e| io_err(format!("read session {:?}: {}", path, e)))?;
    let session: SessionFile = serde_json::from_str(&raw).map_err(|e| {
        stage2_err(
            "corrupt_session",
            &format!("parse session {:?}: {}", path, e),
        )
    })?;
    Ok(Some(session))
}

// Spec §12.3 steps 4-7: serialize + atomic_write. Steps 1-3 (read, validate,
// construct) live in the per-tool `build_*` functions.
pub(crate) fn write_session_atomic(path: &Path, session: &SessionFile) -> Result<(), StageErr> {
    write_json_atomic(path, session).map_err(io_err)?;
    Ok(())
}

// Spec §12.2: completeness checklist. Returns (overall_ok, per_item_map).
pub(crate) fn compute_completeness(transcript: &[SessionTurn]) -> (bool, BTreeMap<String, bool>) {
    let has_q = transcript.iter().any(|t| t.kind == TurnKind::AgentQuestion);
    let has_a = transcript.iter().any(|t| t.kind == TurnKind::UserAnswer);
    let mut m = BTreeMap::new();
    m.insert("at_least_one_agent_question".to_string(), has_q);
    m.insert("at_least_one_user_answer".to_string(), has_a);
    (has_q && has_a, m)
}

// Spec §12.3 "transcript_digest change": sha256 over canonicalized transcript
// array bytes. Returns (hex_digest, canonical_bytes_len).
pub(crate) fn compute_transcript_digest(
    transcript: &[SessionTurn],
) -> Result<(String, usize), StageErr> {
    let bytes = serde_json::to_vec(transcript)
        .map_err(|e| io_err(format!("canonicalize transcript: {}", e)))?;
    let digest = Sha256::digest(&bytes);
    Ok((hex_lower(&digest), bytes.len()))
}

// Lowercase hex, no `0x` prefix. Source: RFC 4648 §8 (Base 16). Hand-rolled
// to keep stage 2's dep footprint to serde + serde_json + sha2 only.
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
