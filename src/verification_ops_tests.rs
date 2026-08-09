//! Stage 2a–2d handlers — start / append / finalize / abort.
//!
//! `verification_ops.rs` had 0% line coverage (issue #169). It is the module
//! that decides whether a finding counts as *verified*, and the receipt it
//! writes is what stage 4 gates on — so an unexercised arm here is a claim
//! about human confirmation that nothing ever checked.
//!
//! The suite is built around the full lifecycle rather than isolated calls,
//! because the interesting failures are sequence failures: finalizing without
//! a round, appending after abort, restarting a live session. Every refusal
//! asserts its reason code, and every write asserts the file on disk — a
//! response that says `verified: true` while no receipt was persisted is the
//! exact shape of the bug this stage exists to prevent.
use super::*;
use std::fs;

const NOW: &str = "2026-07-28T00:00:00Z";

fn tmp_out(tag: &str) -> PathBuf {
    tempfile::Builder::new()
        .prefix(&format!("verification_ops_{tag}_"))
        .tempdir()
        .expect("tempdir")
        .keep()
}

/// A minimal but schema-valid `stage-1.refined.json`, which start_verification
/// requires as its precondition (spec §7.1 (i)).
fn write_refined(output_dir: &Path, run_id: &str, finding_id: &str) {
    let dir = finding_dir_for(output_dir, run_id, finding_id);
    fs::create_dir_all(&dir).expect("mkdir finding dir");
    let refined = json!({
        "extracted": {
            "finding_id": finding_id,
            "title": "a finding",
            "relevance_category": "bug",
            "relevance_score": null,
            "extracted_at": NOW,
            "extractor_version": "test",
            "source_form": "inline",
            "source_path": null,
        },
        "refined_prompt": {"text": "investigate", "role_hint": "engineer"},
        "refinement": {"added_context": [], "orchestrator_version": "test"},
    });
    fs::write(
        dir.join(REFINED_FILE_NAME),
        serde_json::to_vec_pretty(&refined).unwrap(),
    )
    .expect("write refined");
}

fn args_for(output_dir: &Path, extra: Value) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("run_id".into(), json!("run-1"));
    m.insert("finding_id".into(), json!("F-1"));
    m.insert(
        "output_dir".into(),
        json!(output_dir.to_str().expect("utf-8 path")),
    );
    for (k, v) in extra.as_object().expect("object") {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

fn session_at(output_dir: &Path) -> SessionFile {
    read_session(&session_path_for(output_dir, "run-1", "F-1"))
        .expect("read session")
        .expect("session exists")
}

fn sample_session(state: SessionState, transcript: Vec<SessionTurn>) -> SessionFile {
    SessionFile {
        run_id: "run-1".into(),
        finding_id: "F-1".into(),
        state,
        turn_count: transcript.len(),
        started_at: NOW.into(),
        updated_at: NOW.into(),
        schema_ok: true,
        verifier_version: "test".into(),
        transcript,
        aborted_at: None,
        abort_reason: None,
    }
}

fn a_turn(seq: usize, kind: TurnKind) -> SessionTurn {
    SessionTurn {
        seq,
        kind,
        timestamp: NOW.into(),
        content: "text".into(),
        meta: None,
    }
}

// ---------------------------------------------------------------------------
// Argument parsing — shared shape across all four tools
// ---------------------------------------------------------------------------

#[test]
fn every_parser_rejects_non_object_arguments() {
    // Four tools, four spec references, one rule. Each cites its own section
    // so an agent reading the message lands on the right paragraph.
    assert_eq!(
        parse_start_args(&json!(1)).err().expect("start").0,
        "bad_request"
    );
    assert_eq!(
        parse_append_args(&json!(1)).err().expect("append").0,
        "bad_request"
    );
    assert_eq!(
        parse_finalize_args(&json!(1)).err().expect("finalize").0,
        "bad_request"
    );
    assert_eq!(
        parse_abort_args(&json!(1)).err().expect("abort").0,
        "bad_request"
    );
    assert!(parse_start_args(&json!(1))
        .err()
        .expect("start")
        .1
        .contains("§7.1"));
    assert!(parse_append_args(&json!(1))
        .err()
        .expect("append")
        .1
        .contains("§7.2"));
}

#[test]
fn every_parser_requires_the_three_common_fields() {
    for missing in ["run_id", "finding_id", "output_dir"] {
        let mut m = serde_json::Map::new();
        for field in ["run_id", "finding_id", "output_dir"] {
            if field == missing {
                continue;
            }
            m.insert(
                field.into(),
                json!(if field == "output_dir" {
                    "/tmp/o"
                } else {
                    "ok"
                }),
            );
        }
        let args = Value::Object(m);
        assert!(
            parse_start_args(&args).is_err(),
            "start accepted a call missing {missing}"
        );
        assert!(
            parse_finalize_args(&args).is_err(),
            "finalize accepted a call missing {missing}"
        );
        assert!(
            parse_abort_args(&args).is_err(),
            "abort accepted a call missing {missing}"
        );
    }
}

#[test]
fn ids_that_would_escape_the_run_directory_are_rejected() {
    // run_id and finding_id are path segments. `..` here writes a session
    // outside the caller's output_dir.
    for bad in ["../up", "a/b", ""] {
        let args = json!({"run_id": bad, "finding_id": "F-1", "output_dir": "/tmp/o"});
        assert_eq!(
            parse_start_args(&args).err().expect("unsafe run_id").0,
            "unsafe_id",
            "for run_id={bad:?}"
        );
        let args = json!({"run_id": "run-1", "finding_id": bad, "output_dir": "/tmp/o"});
        assert_eq!(
            parse_start_args(&args).err().expect("unsafe finding_id").0,
            "unsafe_id",
            "for finding_id={bad:?}"
        );
    }
}

#[test]
fn output_dir_must_be_absolute() {
    let args = json!({"run_id": "r", "finding_id": "f", "output_dir": "relative/out"});
    let err = parse_start_args(&args).err().expect("relative output_dir");
    assert_eq!(err.0, "bad_request");
    assert!(err.1.contains("absolute"), "{}", err.1);
}

// ---------------------------------------------------------------------------
// append-specific parsing
// ---------------------------------------------------------------------------

#[test]
fn append_accepts_exactly_the_two_turn_kinds() {
    let dir = tmp_out("kinds");
    for (wire, expected) in [
        ("agent_question", TurnKind::AgentQuestion),
        ("user_answer", TurnKind::UserAnswer),
    ] {
        let parsed = parse_append_args(&args_for(&dir, json!({"kind": wire, "content": "c"})))
            .expect("legal kind");
        assert_eq!(parsed.kind, expected);
    }
}

#[test]
fn append_rejects_an_unknown_turn_kind_and_echoes_it() {
    let dir = tmp_out("badkind");
    let err = parse_append_args(&args_for(
        &dir,
        json!({"kind": "assistant_reply", "content": "c"}),
    ))
    .err()
    .expect("unknown kind");
    assert_eq!(err.0, "bad_request");
    assert!(
        err.1.contains("assistant_reply"),
        "the message must echo what was sent, got: {}",
        err.1
    );
    assert!(err.1.contains("agent_question") && err.1.contains("user_answer"));
}

#[test]
fn append_rejects_empty_content() {
    // Spec §5.2 minLength:1. An empty turn would satisfy the alternation
    // invariant while carrying no clarification at all — a verified receipt
    // built on nothing.
    let dir = tmp_out("empty");
    let err = parse_append_args(&args_for(
        &dir,
        json!({"kind": "user_answer", "content": ""}),
    ))
    .err()
    .expect("empty content");
    assert_eq!(err.0, "bad_request");
    assert!(err.1.contains("non-empty"), "{}", err.1);
}

#[test]
fn append_carries_optional_meta_through_untouched() {
    let dir = tmp_out("meta");
    let parsed = parse_append_args(&args_for(
        &dir,
        json!({"kind": "user_answer", "content": "c", "meta": {"source": "cli"}}),
    ))
    .expect("meta is optional but preserved");
    assert_eq!(parsed.meta, Some(json!({"source": "cli"})));

    let without = parse_append_args(&args_for(
        &dir,
        json!({"kind": "user_answer", "content": "c"}),
    ))
    .expect("absent meta");
    assert!(without.meta.is_none());
}

// ---------------------------------------------------------------------------
// check_stage1_refined — the start precondition
// ---------------------------------------------------------------------------

#[test]
fn start_requires_a_stage_1_refined_artifact() {
    let dir = tmp_out("no_refined");
    let finding_dir = finding_dir_for(&dir, "run-1", "F-1");
    fs::create_dir_all(&finding_dir).unwrap();
    let err = check_stage1_refined(&finding_dir).expect_err("no refined artifact");
    assert_eq!(err.0, "no_extraction");
    assert!(
        err.1.contains("refine_finding"),
        "the message must name the tool to call first, got: {}",
        err.1
    );
}

#[test]
fn a_corrupt_refined_artifact_is_distinguished_from_a_missing_one() {
    // Two different remedies: "run refine_finding" versus "your artifact is
    // damaged". Collapsing them sends an agent to re-run a tool that already
    // wrote the broken file.
    let dir = tmp_out("corrupt_refined");
    let finding_dir = finding_dir_for(&dir, "run-1", "F-1");
    fs::create_dir_all(&finding_dir).unwrap();
    fs::write(finding_dir.join(REFINED_FILE_NAME), b"{ not json").unwrap();
    assert_eq!(
        check_stage1_refined(&finding_dir).expect_err("corrupt").0,
        "corrupt_extraction"
    );
}

#[test]
fn valid_json_of_the_wrong_shape_is_a_corrupt_extraction() {
    let dir = tmp_out("wrongshape");
    let finding_dir = finding_dir_for(&dir, "run-1", "F-1");
    fs::create_dir_all(&finding_dir).unwrap();
    fs::write(finding_dir.join(REFINED_FILE_NAME), br#"{"extracted": 1}"#).unwrap();
    assert_eq!(
        check_stage1_refined(&finding_dir)
            .expect_err("wrong shape")
            .0,
        "corrupt_extraction"
    );
}

#[test]
fn an_unreadable_refined_artifact_is_an_io_error() {
    // A directory where the file belongs: exists(), so not the missing arm,
    // and read_to_string fails — the third, distinct outcome.
    let dir = tmp_out("unreadable_refined");
    let finding_dir = finding_dir_for(&dir, "run-1", "F-1");
    fs::create_dir_all(finding_dir.join(REFINED_FILE_NAME)).unwrap();
    assert_eq!(
        check_stage1_refined(&finding_dir)
            .expect_err("unreadable")
            .0,
        "io_error"
    );
}

#[test]
fn a_well_formed_refined_artifact_sets_schema_ok() {
    let dir = tmp_out("good_refined");
    write_refined(&dir, "run-1", "F-1");
    let finding_dir = finding_dir_for(&dir, "run-1", "F-1");
    assert!(check_stage1_refined(&finding_dir).expect("valid artifact"));
}

// ---------------------------------------------------------------------------
// Pure session construction
// ---------------------------------------------------------------------------

#[test]
fn a_new_session_starts_open_and_empty() {
    let dir = tmp_out("new_session");
    let args = parse_start_args(&args_for(&dir, json!({}))).expect("args");
    let session = build_new_session(&args, true, NOW.to_string());
    assert_eq!(session.state, SessionState::Open);
    assert_eq!(session.turn_count, 0);
    assert!(session.transcript.is_empty());
    assert!(session.schema_ok);
    // started_at and updated_at are the same instant on a fresh session; a
    // later updated_at is how a reader spots activity.
    assert_eq!(session.started_at, session.updated_at);
    assert!(session.aborted_at.is_none() && session.abort_reason.is_none());
}

#[test]
fn appending_preserves_history_and_numbers_the_new_turn() {
    let dir = tmp_out("append_pure");
    let prev = sample_session(
        SessionState::WaitingForUser,
        vec![a_turn(0, TurnKind::AgentQuestion)],
    );
    let args = parse_append_args(&args_for(
        &dir,
        json!({"kind": "user_answer", "content": "because X"}),
    ))
    .expect("args");

    let next = build_appended_session(&prev, &args).expect("legal append");
    assert_eq!(next.state, SessionState::WaitingForAgent);
    assert_eq!(next.turn_count, 2);
    assert_eq!(next.transcript.len(), 2);
    // seq is the index of the turn, so the first is 0 — an off-by-one here
    // silently renumbers a whole transcript.
    assert_eq!(next.transcript[1].seq, 1);
    assert_eq!(next.transcript[1].content, "because X");
    // The prior turn survives verbatim: appending must never rewrite history.
    assert_eq!(next.transcript[0].kind, TurnKind::AgentQuestion);
    assert_eq!(next.started_at, prev.started_at, "start time is immutable");
    assert_eq!(next.schema_ok, prev.schema_ok, "schema_ok never mutates");
}

#[test]
fn appending_an_illegal_turn_leaves_the_session_untouched() {
    let dir = tmp_out("append_illegal");
    let prev = sample_session(
        SessionState::WaitingForUser,
        vec![a_turn(0, TurnKind::AgentQuestion)],
    );
    let args = parse_append_args(&args_for(
        &dir,
        json!({"kind": "agent_question", "content": "and another"}),
    ))
    .expect("args");
    assert_eq!(
        build_appended_session(&prev, &args)
            .expect_err("two questions in a row")
            .0,
        "alternation_violation"
    );
}

#[test]
fn aborting_preserves_the_transcript_and_records_why() {
    // Spec §12.3: abort flips state and stamps the time; it does NOT truncate.
    // A cleared transcript would destroy the evidence of what was asked.
    let prev = sample_session(
        SessionState::WaitingForAgent,
        vec![
            a_turn(0, TurnKind::AgentQuestion),
            a_turn(1, TurnKind::UserAnswer),
        ],
    );
    let aborted = build_aborted_session(&prev, Some("user gave up".into()), NOW.into())
        .expect("abort is legal");
    assert_eq!(aborted.state, SessionState::Aborted);
    assert_eq!(aborted.aborted_at.as_deref(), Some(NOW));
    assert_eq!(aborted.abort_reason.as_deref(), Some("user gave up"));
    assert_eq!(aborted.transcript.len(), 2, "transcript must survive");
    assert_eq!(aborted.turn_count, prev.turn_count);
}

#[test]
fn aborting_without_a_reason_leaves_the_field_absent() {
    let prev = sample_session(SessionState::Open, vec![]);
    let aborted = build_aborted_session(&prev, None, NOW.into()).expect("abort");
    assert!(aborted.abort_reason.is_none());
    assert!(
        aborted.aborted_at.is_some(),
        "the timestamp is not optional"
    );
}

#[test]
fn a_finalized_session_cannot_be_aborted() {
    let prev = sample_session(SessionState::Finalized, vec![]);
    assert_eq!(
        build_aborted_session(&prev, None, NOW.into())
            .expect_err("finalized is immutable")
            .0,
        "already_finalized"
    );
}

// ---------------------------------------------------------------------------
// finalize preconditions and the receipt
// ---------------------------------------------------------------------------

#[test]
fn finalize_refuses_a_session_whose_stage_1_artifact_was_broken() {
    // schema_ok is stamped at start and never mutates, so this is the only
    // place a broken stage-1 can still be caught before a receipt claims
    // "verified".
    let mut session = sample_session(SessionState::WaitingForAgent, vec![]);
    session.schema_ok = false;
    assert_eq!(
        check_finalize_preconditions(&session)
            .expect_err("schema_ok false")
            .0,
        "schema_not_ok"
    );
}

#[test]
fn finalize_preconditions_defer_to_the_state_machine() {
    let ok = sample_session(SessionState::WaitingForAgent, vec![]);
    assert!(check_finalize_preconditions(&ok).is_ok());

    for (state, code) in [
        (SessionState::Open, "no_clarification_round"),
        (SessionState::WaitingForUser, "unanswered_question"),
        (SessionState::Finalized, "already_finalized"),
        (SessionState::Aborted, "already_aborted"),
    ] {
        assert_eq!(
            check_finalize_preconditions(&sample_session(state, vec![]))
                .expect_err("illegal finalize")
                .0,
            code,
            "from {state:?}"
        );
    }
}

#[test]
fn the_receipt_is_verified_only_when_every_component_holds() {
    let dir = tmp_out("receipt");
    let args = parse_finalize_args(&args_for(&dir, json!({}))).expect("args");

    let complete = sample_session(
        SessionState::WaitingForAgent,
        vec![
            a_turn(0, TurnKind::AgentQuestion),
            a_turn(1, TurnKind::UserAnswer),
        ],
    );
    let receipt = build_verified_artifact(&complete, &args, NOW.into()).expect("receipt");
    assert!(receipt.verified);
    assert!(receipt.verified_kind.schema_ok);
    assert!(receipt.verified_kind.completeness_ok);
    assert!(
        receipt.verified_kind.user_acknowledged,
        "spec §4: the finalize call itself is the acknowledgement"
    );
    assert_eq!(receipt.digest_algorithm, DIGEST_ALGORITHM);
    assert_eq!(receipt.transcript_digest.len(), 64);
    assert_eq!(receipt.turn_count, 2);
    assert!(receipt.completeness_checklist.values().all(|v| *v));
    // The recorded paths are relative to the run directory, so a receipt
    // copied elsewhere still points at its siblings.
    assert!(receipt.stage1_refined_path.ends_with(REFINED_FILE_NAME));
    assert!(receipt.session_path.ends_with(SESSION_FILE_NAME));
    assert!(receipt.stage1_refined_path.contains("F-1"));
}

#[test]
fn a_one_sided_transcript_yields_an_unverified_receipt() {
    // build_verified_artifact recomputes completeness from the data rather
    // than trusting the state machine that let it through. This is the arm
    // that proves it — a question with no answer is not verification.
    let dir = tmp_out("receipt_partial");
    let args = parse_finalize_args(&args_for(&dir, json!({}))).expect("args");
    let one_sided = sample_session(
        SessionState::WaitingForAgent,
        vec![a_turn(0, TurnKind::AgentQuestion)],
    );
    let receipt = build_verified_artifact(&one_sided, &args, NOW.into()).expect("receipt");
    assert!(!receipt.verified, "no answer means not verified");
    assert!(!receipt.verified_kind.completeness_ok);
    assert!(!receipt.completeness_checklist["at_least_one_user_answer"]);
    assert!(receipt.completeness_checklist["at_least_one_agent_question"]);
}

// ---------------------------------------------------------------------------
// The error envelope
// ---------------------------------------------------------------------------

#[test]
fn the_error_envelope_carries_the_code_as_the_reason() {
    let out = stage2_error_response("no_session".into(), "call start first".into());
    assert_eq!(out["stage"], 2);
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "no_session");
    assert_eq!(out["message"], "call start first");
}

#[test]
fn every_run_wrapper_turns_a_failure_into_that_envelope() {
    for (name, out) in [
        ("start", run_start_verification(&json!(1))),
        ("append", run_append_clarification(&json!(1))),
        ("finalize", run_finalize_verification(&json!(1))),
        ("abort", run_abort_verification(&json!(1))),
    ] {
        assert_eq!(out["status"], "error", "{name}");
        assert_eq!(out["stage"], 2, "{name}");
        assert_eq!(out["reason"], "bad_request", "{name}");
    }
}

// ---------------------------------------------------------------------------
// Handlers against the filesystem — the sequence failures
// ---------------------------------------------------------------------------

#[test]
fn append_finalize_and_abort_refuse_when_no_session_exists() {
    let dir = tmp_out("no_session");
    let append = run_append_clarification(&args_for(
        &dir,
        json!({"kind": "agent_question", "content": "c"}),
    ));
    assert_eq!(append["reason"], "no_session", "{append}");
    assert_eq!(
        run_finalize_verification(&args_for(&dir, json!({})))["reason"],
        "no_session"
    );
    assert_eq!(
        run_abort_verification(&args_for(&dir, json!({})))["reason"],
        "no_session"
    );
}

#[test]
fn start_writes_an_open_session_to_disk() {
    let dir = tmp_out("start_ok");
    write_refined(&dir, "run-1", "F-1");

    let out = run_start_verification(&args_for(&dir, json!({})));
    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["state"], "open");
    assert_eq!(out["run_id"], "run-1");

    // The response promises a path; a session must actually be there.
    let promised = Path::new(out["session_path"].as_str().expect("session_path"));
    assert!(promised.exists(), "no session at {promised:?}");
    assert_eq!(session_at(&dir).state, SessionState::Open);
}

#[test]
fn start_refuses_a_finding_with_no_refined_artifact() {
    let dir = tmp_out("start_no_refined");
    let out = run_start_verification(&args_for(&dir, json!({})));
    assert_eq!(out["reason"], "no_extraction", "{out}");
}

#[test]
fn start_refuses_to_restart_a_live_session() {
    // The destructive case: a second start on an open session would discard a
    // transcript the user already answered into.
    let dir = tmp_out("restart_live");
    write_refined(&dir, "run-1", "F-1");
    assert_eq!(
        run_start_verification(&args_for(&dir, json!({})))["status"],
        "ok"
    );

    let again = run_start_verification(&args_for(&dir, json!({})));
    assert_eq!(again["reason"], "illegal_transition", "{again}");
}

#[test]
fn an_aborted_session_can_be_restarted_and_the_transcript_resets() {
    let dir = tmp_out("restart_aborted");
    write_refined(&dir, "run-1", "F-1");
    run_start_verification(&args_for(&dir, json!({})));
    run_append_clarification(&args_for(
        &dir,
        json!({"kind": "agent_question", "content": "q"}),
    ));
    assert_eq!(
        run_abort_verification(&args_for(&dir, json!({"reason": "stale"})))["status"],
        "ok"
    );
    assert_eq!(session_at(&dir).transcript.len(), 1, "abort keeps history");

    let restarted = run_start_verification(&args_for(&dir, json!({})));
    assert_eq!(restarted["status"], "ok", "{restarted}");
    assert_eq!(restarted["state"], "open");
    let after = session_at(&dir);
    assert!(
        after.transcript.is_empty(),
        "a restart begins a new transcript, got {} turns",
        after.transcript.len()
    );
    assert!(after.aborted_at.is_none(), "restart clears the abort stamp");
}

#[test]
fn the_full_lifecycle_produces_a_verified_receipt_and_an_index_entry() {
    let dir = tmp_out("lifecycle");
    write_refined(&dir, "run-1", "F-1");

    assert_eq!(
        run_start_verification(&args_for(&dir, json!({})))["status"],
        "ok"
    );
    let q = run_append_clarification(&args_for(
        &dir,
        json!({"kind": "agent_question", "content": "which module?"}),
    ));
    assert_eq!(q["status"], "ok", "{q}");
    assert_eq!(q["state"], "waiting_for_user");
    assert_eq!(q["seq"], 0);

    let a = run_append_clarification(&args_for(
        &dir,
        json!({"kind": "user_answer", "content": "the resolver"}),
    ));
    assert_eq!(a["state"], "waiting_for_agent");
    assert_eq!(a["seq"], 1);
    assert_eq!(a["turn_count"], 2);

    let done = run_finalize_verification(&args_for(&dir, json!({})));
    assert_eq!(done["status"], "ok", "{done}");
    assert_eq!(done["state"], "finalized");
    assert_eq!(done["verified"], json!(true));
    assert_eq!(done["turn_count"], 2);
    assert!(done["bytes_written"].as_u64().unwrap() > 0);
    assert_eq!(done["digest_algorithm"], DIGEST_ALGORITHM);

    // The receipt is the artifact stage 4 gates on: it must exist on disk.
    let receipt_path = Path::new(done["verified_path"].as_str().expect("verified_path"));
    assert!(receipt_path.exists(), "no receipt at {receipt_path:?}");
    let receipt: Value =
        serde_json::from_slice(&fs::read(receipt_path).unwrap()).expect("receipt parses");
    assert_eq!(receipt["verified"], json!(true));
    assert_eq!(receipt["transcript_digest"], done["transcript_digest"]);

    // The session is flipped, so a second finalize is refused rather than
    // silently rewriting the receipt.
    assert_eq!(session_at(&dir).state, SessionState::Finalized);
    let twice = run_finalize_verification(&args_for(&dir, json!({})));
    assert_eq!(twice["reason"], "already_finalized", "{twice}");

    // …and the run index records the verification for downstream stages.
    let index = dir.join(RUNS_DIR_NAME).join("run-1").join(INDEX_FILE_NAME);
    assert!(index.exists(), "no index at {index:?}");
    let index_text = fs::read_to_string(&index).unwrap();
    assert!(index_text.contains("verified"), "index: {index_text}");
}

#[test]
fn finalizing_before_the_user_answers_is_refused() {
    let dir = tmp_out("finalize_early");
    write_refined(&dir, "run-1", "F-1");
    run_start_verification(&args_for(&dir, json!({})));

    // Nothing asked yet.
    assert_eq!(
        run_finalize_verification(&args_for(&dir, json!({})))["reason"],
        "no_clarification_round"
    );

    // Asked, not yet answered.
    run_append_clarification(&args_for(
        &dir,
        json!({"kind": "agent_question", "content": "q"}),
    ));
    let out = run_finalize_verification(&args_for(&dir, json!({})));
    assert_eq!(out["reason"], "unanswered_question", "{out}");

    // Nothing was written on either refusal.
    let receipt = finding_dir_for(&dir, "run-1", "F-1").join(VERIFIED_FILE_NAME);
    assert!(
        !receipt.exists(),
        "a refused finalize must not leave a receipt"
    );
}

#[test]
fn appending_to_an_aborted_session_is_refused() {
    let dir = tmp_out("append_aborted");
    write_refined(&dir, "run-1", "F-1");
    run_start_verification(&args_for(&dir, json!({})));
    run_abort_verification(&args_for(&dir, json!({})));

    let out = run_append_clarification(&args_for(
        &dir,
        json!({"kind": "agent_question", "content": "q"}),
    ));
    assert_eq!(out["reason"], "already_aborted", "{out}");
}

#[test]
fn abort_reports_the_state_it_reached_and_persists_it() {
    let dir = tmp_out("abort_ok");
    write_refined(&dir, "run-1", "F-1");
    run_start_verification(&args_for(&dir, json!({})));

    let out = run_abort_verification(&args_for(&dir, json!({"reason": "duplicate finding"})));
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(out["state"], "aborted");
    assert_eq!(out["run_id"], "run-1");
    assert!(out["aborted_at"].is_string());

    let persisted = session_at(&dir);
    assert_eq!(persisted.state, SessionState::Aborted);
    assert_eq!(persisted.abort_reason.as_deref(), Some("duplicate finding"));
}
