//! Stage 2a-2d — `start_verification` / `append_clarification` /
//! `finalize_verification` / `abort_verification` handler logic.
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

use crate::finding_index::*;
use crate::finding_refine::*;
use crate::handler_util::*;
use crate::verification_core::*;

// ---------------------------------------------------------------------------
// Stage 2a — start_verification (spec §7.1 + §12.1 restart)
// ---------------------------------------------------------------------------

pub(crate) struct StartArgs {
    pub(crate) run_id: String,
    pub(crate) finding_id: String,
    pub(crate) output_dir: PathBuf,
}

pub(crate) fn parse_start_args(arguments: &Value) -> Result<StartArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §7.1)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    Ok(StartArgs {
        run_id,
        finding_id,
        output_dir,
    })
}

// Spec §7.1 precondition (i): stage-1.refined.json exists and parses. Also
// sets `schema_ok`, which never mutates after start.
pub(crate) fn check_stage1_refined(finding_dir: &Path) -> Result<bool, StageErr> {
    let refined_path = finding_dir.join(REFINED_FILE_NAME);
    if !refined_path.exists() {
        return Err(stage2_err(
            "no_extraction",
            &format!(
                "no {} at {:?} — call refine_finding first (spec §4 schema_ok, §7.1)",
                REFINED_FILE_NAME, refined_path
            ),
        ));
    }
    let raw = fs::read_to_string(&refined_path)
        .map_err(|e| io_err(format!("read {:?}: {}", refined_path, e)))?;
    let _parsed: RefinedArtifact = serde_json::from_str(&raw).map_err(|e| {
        stage2_err(
            "corrupt_extraction",
            &format!("parse {:?}: {}", refined_path, e),
        )
    })?;
    Ok(true)
}

// Pure construction per §12.3 step 3 (never mutate in place).
pub(crate) fn build_new_session(args: &StartArgs, schema_ok: bool, now: String) -> SessionFile {
    SessionFile {
        run_id: args.run_id.clone(),
        finding_id: args.finding_id.clone(),
        state: SessionState::Open,
        turn_count: 0,
        started_at: now.clone(),
        updated_at: now,
        schema_ok,
        verifier_version: VERIFIER_VERSION.to_string(),
        transcript: Vec::new(),
        aborted_at: None,
        abort_reason: None,
    }
}

pub(crate) fn do_start_verification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_start_args(arguments)?;
    let finding_dir = finding_dir_for(&args.output_dir, &args.run_id, &args.finding_id);
    let schema_ok = check_stage1_refined(&finding_dir)?;
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    if let Some(existing) = read_session(&session_path)? {
        // Spec §7.1 precondition (ii) + §12.1: only aborted is restartable.
        let _ = can_transition(existing.state, TransitionEvent::Start)?;
    }
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let session = build_new_session(&args, schema_ok, now.clone());
    fs::create_dir_all(&finding_dir)
        .map_err(|e| io_err(format!("mkdir {:?}: {}", finding_dir, e)))?;
    write_session_atomic(&session_path, &session)?;
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": session.state.as_str(),
        "run_id": session.run_id,
        "finding_id": session.finding_id,
        "started_at": session.started_at,
        "session_path": session_path.to_string_lossy(),
    }))
}

pub(crate) fn run_start_verification(arguments: &Value) -> Value {
    match do_start_verification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

pub(crate) fn stage2_error_response(code: String, msg: String) -> Value {
    json!({
        "stage": 2,
        "status": "error",
        "reason": code,
        "message": msg,
    })
}

// ---------------------------------------------------------------------------
// Stage 2b — append_clarification (spec §7.2 + §12.3 single-file rewrite)
// ---------------------------------------------------------------------------

pub(crate) struct AppendArgs {
    pub(crate) run_id: String,
    pub(crate) finding_id: String,
    pub(crate) output_dir: PathBuf,
    pub(crate) kind: TurnKind,
    pub(crate) content: String,
    pub(crate) meta: Option<Value>,
}

pub(crate) fn parse_append_args(arguments: &Value) -> Result<AppendArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §7.2)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    let kind_str = require_string_arg(args, "kind")?;
    let kind = match kind_str.as_str() {
        "agent_question" => TurnKind::AgentQuestion,
        "user_answer" => TurnKind::UserAnswer,
        other => {
            return Err(bad_request(format!(
                "kind must be 'agent_question' or 'user_answer' (spec §5.2, §7.2): got {:?}",
                other
            )))
        }
    };
    let content = require_string_arg(args, "content")?;
    if content.is_empty() {
        return Err(bad_request(
            "content must be non-empty (spec §5.2 minLength:1)",
        ));
    }
    let meta = args.get("meta").cloned();
    Ok(AppendArgs {
        run_id,
        finding_id,
        output_dir,
        kind,
        content,
        meta,
    })
}

// Pure construction of the new session from the old one + the incoming turn.
// Spec §12.3 step 3: "never mutate in place" — returns a fresh SessionFile.
pub(crate) fn build_appended_session(
    prev: &SessionFile,
    args: &AppendArgs,
) -> Result<SessionFile, StageErr> {
    let event = match args.kind {
        TurnKind::AgentQuestion => TransitionEvent::AgentQuestion,
        TurnKind::UserAnswer => TransitionEvent::UserAnswer,
    };
    let new_state = can_transition(prev.state, event)?;
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let mut transcript = prev.transcript.clone();
    let new_turn = SessionTurn {
        seq: transcript.len(),
        kind: args.kind,
        timestamp: now.clone(),
        content: args.content.clone(),
        meta: args.meta.clone(),
    };
    transcript.push(new_turn);
    Ok(SessionFile {
        run_id: prev.run_id.clone(),
        finding_id: prev.finding_id.clone(),
        state: new_state,
        turn_count: transcript.len(),
        started_at: prev.started_at.clone(),
        updated_at: now,
        schema_ok: prev.schema_ok,
        verifier_version: prev.verifier_version.clone(),
        transcript,
        aborted_at: None,
        abort_reason: None,
    })
}

pub(crate) fn do_append_clarification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_append_args(arguments)?;
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    let prev = read_session(&session_path)?.ok_or_else(|| {
        stage2_err(
            "no_session",
            "no stage-2.session.json — call start_verification first (spec §7.2)",
        )
    })?;
    let next = build_appended_session(&prev, &args)?;
    write_session_atomic(&session_path, &next)?;
    let last_seq = next.transcript.len().saturating_sub(1);
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": next.state.as_str(),
        "seq": last_seq,
        "turn_count": next.turn_count,
    }))
}

pub(crate) fn run_append_clarification(arguments: &Value) -> Value {
    match do_append_clarification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

// ---------------------------------------------------------------------------
// Stage 2c — finalize_verification (spec §7.3 + §12.2 + §12.3)
// ---------------------------------------------------------------------------

pub(crate) struct FinalizeArgs {
    pub(crate) run_id: String,
    pub(crate) finding_id: String,
    pub(crate) output_dir: PathBuf,
}

pub(crate) fn parse_finalize_args(arguments: &Value) -> Result<FinalizeArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §7.3)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    Ok(FinalizeArgs {
        run_id,
        finding_id,
        output_dir,
    })
}

pub(crate) fn load_session_for_finalize(
    args: &FinalizeArgs,
) -> Result<(SessionFile, PathBuf), StageErr> {
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    let session = read_session(&session_path)?.ok_or_else(|| {
        stage2_err(
            "no_session",
            "no stage-2.session.json — call start_verification first (spec §7.3)",
        )
    })?;
    Ok((session, session_path))
}

// Spec §12.2: finalize rejects from `open` and `waiting_for_user`. Also
// enforces §7.3 precondition (ii): schema_ok must be true.
pub(crate) fn check_finalize_preconditions(session: &SessionFile) -> Result<(), StageErr> {
    if !session.schema_ok {
        return Err(stage2_err(
            "schema_not_ok",
            "session.schema_ok is false; stage-1 artifact is broken (spec §7.3 (ii))",
        ));
    }
    let _ = can_transition(session.state, TransitionEvent::Finalize)?;
    Ok(())
}

// Spec §12.3: digest over the transcript array only (NOT the whole session
// file — that would include `state: "finalized"` which is circular).
pub(crate) fn build_verified_artifact(
    session: &SessionFile,
    args: &FinalizeArgs,
    now: String,
) -> Result<VerifiedArtifact, StageErr> {
    let (completeness_ok, checklist) = compute_completeness(&session.transcript);
    // §12.2 guarantees this is true if we got past check_finalize_preconditions,
    // but compute it from data anyway — no invented invariants.
    let (digest_hex, digest_input_len) = compute_transcript_digest(&session.transcript)?;
    let user_acknowledged = true; // Spec §4: the finalize tool call itself is the signal.
    let verified = session.schema_ok && completeness_ok && user_acknowledged;
    Ok(VerifiedArtifact {
        run_id: session.run_id.clone(),
        finding_id: session.finding_id.clone(),
        verified,
        verified_kind: VerifiedKind {
            schema_ok: session.schema_ok,
            completeness_ok,
            user_acknowledged,
        },
        finalized_at: now,
        stage1_refined_path: format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, args.finding_id, REFINED_FILE_NAME
        ),
        session_path: format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, args.finding_id, SESSION_FILE_NAME
        ),
        transcript_digest: digest_hex,
        digest_algorithm: DIGEST_ALGORITHM.to_string(),
        transcript_bytes_at_finalize: digest_input_len,
        turn_count: session.turn_count,
        verifier_version: VERIFIER_VERSION.to_string(),
        completeness_checklist: checklist,
    })
}

// Spec §12.3: write verified artifact, flip session.state to finalized
// atomically (whole-file rewrite), upsert index entry.
//
// Crash window: if we crash between the verified write and the session flip,
// the verified receipt is on disk but the session is still waiting_for_agent.
// Re-running finalize is idempotent (same transcript → same digest).
pub(crate) fn persist_finalize(
    args: &FinalizeArgs,
    session: &SessionFile,
    verified: &VerifiedArtifact,
    now: String,
) -> Result<(PathBuf, usize), StageErr> {
    let finding_dir = finding_dir_for(&args.output_dir, &args.run_id, &args.finding_id);
    let verified_path = finding_dir.join(VERIFIED_FILE_NAME);
    let bytes_written = write_json_atomic(&verified_path, verified).map_err(io_err)?;
    let finalized = SessionFile {
        state: SessionState::Finalized,
        updated_at: now.clone(),
        ..session.clone()
    };
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    write_session_atomic(&session_path, &finalized)?;
    upsert_verified_index(args, verified, now)?;
    Ok((verified_path, bytes_written))
}

// Spec §5.4: stage-2 index fields. Preserves the refined_* fields stage 1b
// wrote. artifact_path is taken from the existing entry (refined path).
pub(crate) fn upsert_verified_index(
    args: &FinalizeArgs,
    verified: &VerifiedArtifact,
    now: String,
) -> Result<(), StageErr> {
    let entry = IndexEntry {
        // Will be overridden by PreserveRefinedOnly; put a sentinel here.
        artifact_path: String::new(),
        extractor_version: EXTRACTOR_VERSION.to_string(),
        verified_at: Some(now),
        verified: Some(verified.verified),
        stage2_path: Some(format!(
            "{}/{}/{}",
            FINDINGS_DIR_NAME, args.finding_id, VERIFIED_FILE_NAME
        )),
        ..IndexEntry::default()
    };
    upsert_index_entry(
        &args.output_dir,
        &args.run_id,
        &args.finding_id,
        entry,
        MergeMode::PreserveRefinedOnly,
    )
    .map_err(io_err)
}

pub(crate) fn do_finalize_verification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_finalize_args(arguments)?;
    let (session, _session_path) = load_session_for_finalize(&args)?;
    check_finalize_preconditions(&session)?;
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let verified = build_verified_artifact(&session, &args, now.clone())?;
    let (verified_path, bytes_written) = persist_finalize(&args, &session, &verified, now)?;
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": SessionState::Finalized.as_str(),
        "verified": verified.verified,
        "verified_kind": {
            "schema_ok": verified.verified_kind.schema_ok,
            "completeness_ok": verified.verified_kind.completeness_ok,
            "user_acknowledged": verified.verified_kind.user_acknowledged,
        },
        "verified_path": verified_path.to_string_lossy(),
        "turn_count": verified.turn_count,
        "transcript_digest": verified.transcript_digest,
        "digest_algorithm": verified.digest_algorithm,
        "transcript_bytes_at_finalize": verified.transcript_bytes_at_finalize,
        "bytes_written": bytes_written,
        "verifier_version": VERIFIER_VERSION,
    }))
}

pub(crate) fn run_finalize_verification(arguments: &Value) -> Value {
    match do_finalize_verification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}

// ---------------------------------------------------------------------------
// Stage 2d — abort_verification (spec §12.1)
// ---------------------------------------------------------------------------

pub(crate) struct AbortArgs {
    pub(crate) run_id: String,
    pub(crate) finding_id: String,
    pub(crate) output_dir: PathBuf,
    pub(crate) reason: Option<String>,
}

pub(crate) fn parse_abort_args(arguments: &Value) -> Result<AbortArgs, StageErr> {
    let args = arguments
        .as_object()
        .ok_or_else(|| bad_request("arguments must be an object (spec §12.1)"))?;
    let run_id = require_string_arg(args, "run_id")?;
    validate_safe_id("run_id", &run_id).map_err(unsafe_id_err)?;
    let finding_id = require_string_arg(args, "finding_id")?;
    validate_safe_id("finding_id", &finding_id).map_err(unsafe_id_err)?;
    let output_dir_str = require_string_arg(args, "output_dir")?;
    let output_dir = require_absolute(&output_dir_str, "output_dir").map_err(bad_request)?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(AbortArgs {
        run_id,
        finding_id,
        output_dir,
        reason,
    })
}

// Pure construction. Transcript is preserved verbatim (spec §12.3 "abort
// does not truncate the transcript; it only flips state and sets aborted_at").
pub(crate) fn build_aborted_session(
    prev: &SessionFile,
    reason: Option<String>,
    now: String,
) -> Result<SessionFile, StageErr> {
    let _ = can_transition(prev.state, TransitionEvent::Abort)?;
    Ok(SessionFile {
        state: SessionState::Aborted,
        updated_at: now.clone(),
        aborted_at: Some(now),
        abort_reason: reason,
        ..prev.clone()
    })
}

pub(crate) fn do_abort_verification(arguments: &Value) -> Result<Value, StageErr> {
    let args = parse_abort_args(arguments)?;
    let session_path = session_path_for(&args.output_dir, &args.run_id, &args.finding_id);
    let prev = read_session(&session_path)?.ok_or_else(|| {
        stage2_err(
            "no_session",
            "no stage-2.session.json to abort (spec §12.1)",
        )
    })?;
    let now = format_iso8601_utc(now_unix_seconds_nanos().0);
    let aborted = build_aborted_session(&prev, args.reason.clone(), now.clone())?;
    write_session_atomic(&session_path, &aborted)?;
    Ok(json!({
        "stage": 2,
        "status": "ok",
        "state": aborted.state.as_str(),
        "run_id": aborted.run_id,
        "finding_id": aborted.finding_id,
        "turn_count": aborted.turn_count,
        "aborted_at": now,
    }))
}

pub(crate) fn run_abort_verification(arguments: &Value) -> Value {
    match do_abort_verification(arguments) {
        Ok(v) => v,
        Err((code, msg)) => stage2_error_response(code, msg),
    }
}
