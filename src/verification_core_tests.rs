//! Stage 2 session core — state machine, session I/O, completeness and digest.
//!
//! `verification_core.rs` had 0% line coverage (issue #169) while being the
//! arbiter of every legal and illegal move in a verification session. The
//! state machine is where "you cannot finalize a session that never asked a
//! question" is decided, so every arm here asserts the OBSERVABLE effect — the
//! resulting state, or the reason code a caller surfaces to an agent — rather
//! than only that a `Result` was an `Err`. A wrong-but-present reason code is
//! how an agent gets told the wrong thing to do next.
use super::*;
use std::fs;
use std::path::Path;

fn turn(seq: usize, kind: TurnKind, content: &str) -> SessionTurn {
    SessionTurn {
        seq,
        kind,
        timestamp: "2026-07-28T00:00:00Z".to_string(),
        content: content.to_string(),
        meta: None,
    }
}

fn session_with(state: SessionState, transcript: Vec<SessionTurn>) -> SessionFile {
    SessionFile {
        run_id: "run-1".to_string(),
        finding_id: "f-1".to_string(),
        state,
        turn_count: transcript.len(),
        started_at: "2026-07-28T00:00:00Z".to_string(),
        updated_at: "2026-07-28T00:00:01Z".to_string(),
        schema_ok: true,
        verifier_version: "test".to_string(),
        transcript,
        aborted_at: None,
        abort_reason: None,
    }
}

const ALL_STATES: [SessionState; 5] = [
    SessionState::Open,
    SessionState::WaitingForUser,
    SessionState::WaitingForAgent,
    SessionState::Finalized,
    SessionState::Aborted,
];

// ---------------------------------------------------------------------------
// State naming — the wire vocabulary
// ---------------------------------------------------------------------------

#[test]
fn state_names_are_the_snake_case_wire_strings() {
    // These strings are the session file's on-disk vocabulary and the reason
    // an old session still parses. Renaming one silently orphans every
    // session written before the rename.
    assert_eq!(SessionState::Open.as_str(), "open");
    assert_eq!(SessionState::WaitingForUser.as_str(), "waiting_for_user");
    assert_eq!(SessionState::WaitingForAgent.as_str(), "waiting_for_agent");
    assert_eq!(SessionState::Finalized.as_str(), "finalized");
    assert_eq!(SessionState::Aborted.as_str(), "aborted");
}

#[test]
fn state_serialises_to_the_same_string_as_as_str() {
    // as_str and serde must not drift apart: one writes the file, the other
    // is what a reader compares against.
    for state in ALL_STATES {
        let json = serde_json::to_string(&state).expect("state serialises");
        assert_eq!(json, format!("\"{}\"", state.as_str()));
    }
}

// ---------------------------------------------------------------------------
// Terminal states — the two rejections that guard every other transition
// ---------------------------------------------------------------------------

#[test]
fn finalized_rejects_every_event_with_already_finalized() {
    // Spec §8.4: a finalized session is immutable. Including Start — an
    // aborted session may be restarted, a finalized one may not, and that
    // asymmetry is the whole point of having two terminal states.
    for event in [
        TransitionEvent::Start,
        TransitionEvent::AgentQuestion,
        TransitionEvent::UserAnswer,
        TransitionEvent::Finalize,
        TransitionEvent::Abort,
    ] {
        let err = can_transition(SessionState::Finalized, event)
            .expect_err("finalized must reject every event");
        assert_eq!(err.0, "already_finalized", "event {:?}", event);
    }
}

#[test]
fn aborted_rejects_everything_except_start() {
    for event in [
        TransitionEvent::AgentQuestion,
        TransitionEvent::UserAnswer,
        TransitionEvent::Finalize,
        TransitionEvent::Abort,
    ] {
        let err = can_transition(SessionState::Aborted, event)
            .expect_err("aborted must reject non-start events");
        assert_eq!(err.0, "already_aborted", "event {:?}", event);
    }
}

#[test]
fn aborted_can_be_restarted() {
    // Spec §12.1: this is the one door out of a terminal state.
    assert_eq!(
        can_transition(SessionState::Aborted, TransitionEvent::Start).expect("restart is legal"),
        SessionState::Open
    );
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

#[test]
fn start_is_illegal_on_a_live_session() {
    // Spec §7.1: an in-flight session is not silently reset — restarting one
    // would discard a transcript a user already answered into.
    for from in [
        SessionState::Open,
        SessionState::WaitingForUser,
        SessionState::WaitingForAgent,
    ] {
        let err = transition_start(from).expect_err("start on a live session is illegal");
        assert_eq!(err.0, "illegal_transition", "from {:?}", from);
        assert!(
            err.1.contains("aborted session"),
            "the message must say what WOULD be legal, got: {}",
            err.1
        );
    }
}

// ---------------------------------------------------------------------------
// append — the alternation invariant
// ---------------------------------------------------------------------------

#[test]
fn append_walks_the_question_answer_alternation() {
    assert_eq!(
        transition_append(SessionState::Open, TurnKind::AgentQuestion).expect("first question"),
        SessionState::WaitingForUser
    );
    assert_eq!(
        transition_append(SessionState::WaitingForUser, TurnKind::UserAnswer).expect("answer"),
        SessionState::WaitingForAgent
    );
    assert_eq!(
        transition_append(SessionState::WaitingForAgent, TurnKind::AgentQuestion)
            .expect("next question"),
        SessionState::WaitingForUser
    );
}

#[test]
fn append_rejects_two_consecutive_turns_of_the_same_kind() {
    // Spec §3: the alternation invariant. Without it a "verification" can be
    // an agent talking to itself, which is exactly what stage 2 exists to
    // prevent.
    for (from, kind) in [
        (SessionState::Open, TurnKind::UserAnswer),
        (SessionState::WaitingForUser, TurnKind::AgentQuestion),
        (SessionState::WaitingForAgent, TurnKind::UserAnswer),
    ] {
        let err = transition_append(from, kind).expect_err("alternation must be enforced");
        assert_eq!(err.0, "alternation_violation", "from {:?} {:?}", from, kind);
    }
}

#[test]
fn can_transition_routes_both_turn_kinds_to_append() {
    // The dispatch itself is a path: routing UserAnswer to the question
    // handler would accept an illegal transcript while every unit test of
    // `transition_append` still passed.
    assert_eq!(
        can_transition(SessionState::Open, TransitionEvent::AgentQuestion).expect("question"),
        SessionState::WaitingForUser
    );
    assert_eq!(
        can_transition(SessionState::WaitingForUser, TransitionEvent::UserAnswer).expect("answer"),
        SessionState::WaitingForAgent
    );
    assert_eq!(
        can_transition(SessionState::Open, TransitionEvent::UserAnswer)
            .expect_err("answer before any question")
            .0,
        "alternation_violation"
    );
}

// ---------------------------------------------------------------------------
// finalize — three distinct refusals, and why they must stay distinct
// ---------------------------------------------------------------------------

#[test]
fn finalize_succeeds_only_after_a_completed_round() {
    assert_eq!(
        transition_finalize(SessionState::WaitingForAgent).expect("finalize after an answer"),
        SessionState::Finalized
    );
}

#[test]
fn finalize_refusals_carry_distinct_actionable_codes() {
    // One generic "illegal" code would tell an agent to give up; these three
    // tell it what to DO — ask a question, wait for the answer, or stop.
    assert_eq!(
        transition_finalize(SessionState::Open)
            .expect_err("no round yet")
            .0,
        "no_clarification_round"
    );
    assert_eq!(
        transition_finalize(SessionState::WaitingForUser)
            .expect_err("question outstanding")
            .0,
        "unanswered_question"
    );
    // The catch-all arm. Unreachable through `can_transition` (terminal
    // states are rejected earlier), so this is the only test that reaches it —
    // and it must still refuse rather than fall through to Ok.
    for from in [SessionState::Finalized, SessionState::Aborted] {
        assert_eq!(
            transition_finalize(from).expect_err("terminal state").0,
            "illegal_transition",
            "from {:?}",
            from
        );
    }
}

// ---------------------------------------------------------------------------
// abort
// ---------------------------------------------------------------------------

#[test]
fn abort_is_legal_from_every_non_terminal_state() {
    for from in [
        SessionState::Open,
        SessionState::WaitingForUser,
        SessionState::WaitingForAgent,
    ] {
        assert_eq!(
            can_transition(from, TransitionEvent::Abort).expect("abort is always available"),
            SessionState::Aborted,
            "from {:?}",
            from
        );
    }
}

#[test]
fn stage2_err_carries_the_code_and_message_verbatim() {
    let err = stage2_err("some_code", "some message");
    assert_eq!(err, ("some_code".to_string(), "some message".to_string()));
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

#[test]
fn session_and_finding_paths_agree_on_the_layout() {
    let root = Path::new("/out");
    let finding = finding_dir_for(root, "run-7", "F-3");
    let session = session_path_for(root, "run-7", "F-3");
    assert_eq!(finding, Path::new("/out/runs/run-7/findings/F-3"));
    assert_eq!(
        session,
        Path::new("/out/runs/run-7/findings/F-3/stage-2.session.json")
    );
    // The session file must live INSIDE the finding directory, or a finalize
    // writes its receipt somewhere the next reader does not look.
    assert_eq!(session.parent(), Some(finding.as_path()));
}

// ---------------------------------------------------------------------------
// Session I/O
// ---------------------------------------------------------------------------

#[test]
fn reading_a_missing_session_is_none_not_an_error() {
    // start_verification distinguishes "no session yet" (legal) from "unreadable
    // session" (an error). Collapsing the two would make every first start fail.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nope.json");
    assert!(read_session(&path)
        .expect("missing is not an error")
        .is_none());
}

#[test]
fn a_written_session_round_trips_through_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_path_for(dir.path(), "run-1", "f-1");
    let session = session_with(
        SessionState::WaitingForUser,
        vec![turn(1, TurnKind::AgentQuestion, "which module?")],
    );

    write_session_atomic(&path, &session).expect("write succeeds");

    let read = read_session(&path)
        .expect("read succeeds")
        .expect("session is present");
    assert_eq!(read.state, SessionState::WaitingForUser);
    assert_eq!(read.run_id, "run-1");
    assert_eq!(read.transcript.len(), 1);
    assert_eq!(read.transcript[0].content, "which module?");
    assert_eq!(read.transcript[0].kind, TurnKind::AgentQuestion);
}

#[test]
fn write_creates_the_finding_directory_it_needs() {
    // The finding directory does not exist on a first start; the write must
    // make it rather than failing on a path the caller never created.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_path_for(dir.path(), "run-new", "f-new");
    assert!(!path.parent().expect("has parent").exists());
    write_session_atomic(&path, &session_with(SessionState::Open, vec![])).expect("write succeeds");
    assert!(path.exists());
}

#[test]
fn absent_optional_fields_stay_absent_on_disk() {
    // `aborted_at`/`abort_reason` are skip_serializing_if None. A live session
    // carrying a null `abort_reason` reads as aborted-with-no-reason to a
    // consumer that checks for the key's presence.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("s.json");
    write_session_atomic(&path, &session_with(SessionState::Open, vec![])).expect("write");
    let raw = fs::read_to_string(&path).expect("read raw");
    assert!(!raw.contains("aborted_at"), "raw: {}", raw);
    assert!(!raw.contains("abort_reason"), "raw: {}", raw);
}

#[test]
fn a_corrupt_session_is_reported_as_corrupt_not_missing() {
    // The dangerous confusion: if a truncated session read as `None`, a start
    // would overwrite a transcript instead of refusing.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("s.json");
    fs::write(&path, b"{ this is not json").expect("write garbage");
    let err = read_session(&path).expect_err("corrupt session must not read as missing");
    assert_eq!(err.0, "corrupt_session");
    assert!(
        err.1.contains("s.json"),
        "the message must name the file, got: {}",
        err.1
    );
}

#[test]
fn valid_json_of_the_wrong_shape_is_also_corrupt() {
    // Parses as JSON, is not a session. Distinguishes "is it JSON?" from
    // "is it a SessionFile?" — only the second is the real contract.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("s.json");
    fs::write(&path, br#"{"run_id":"r"}"#).expect("write partial");
    assert_eq!(
        read_session(&path).expect_err("missing fields").0,
        "corrupt_session"
    );
}

#[test]
fn an_unreadable_session_path_is_an_io_error() {
    // A directory where a file is expected: exists(), so it is not the None
    // arm, and read_to_string fails — the io_err arm, distinct from corruption.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session-as-a-directory");
    fs::create_dir(&path).expect("mkdir");
    let err = read_session(&path).expect_err("reading a directory must fail");
    assert_eq!(err.0, "io_error");
}

// ---------------------------------------------------------------------------
// Completeness
// ---------------------------------------------------------------------------

#[test]
fn completeness_needs_both_a_question_and_an_answer() {
    let q = turn(1, TurnKind::AgentQuestion, "q");
    let a = turn(2, TurnKind::UserAnswer, "a");

    let (ok, items) = compute_completeness(&[]);
    assert!(!ok, "an empty transcript is not complete");
    assert!(!items["at_least_one_agent_question"]);
    assert!(!items["at_least_one_user_answer"]);

    let (ok, items) = compute_completeness(std::slice::from_ref(&q));
    assert!(!ok, "a question with no answer is not complete");
    assert!(items["at_least_one_agent_question"]);
    assert!(!items["at_least_one_user_answer"]);

    let (ok, items) = compute_completeness(std::slice::from_ref(&a));
    assert!(!ok, "an answer with no question is not complete");
    assert!(!items["at_least_one_agent_question"]);
    assert!(items["at_least_one_user_answer"]);

    let (ok, items) = compute_completeness(&[q, a]);
    assert!(ok, "one of each is complete");
    assert!(items.values().all(|v| *v));
}

#[test]
fn the_checklist_names_which_item_failed() {
    // Spec §9 item 6: a bare `false` sends a maintainer back to the transcript
    // to work out which half is missing.
    let (_, items) = compute_completeness(&[]);
    let keys: Vec<_> = items.keys().cloned().collect();
    assert_eq!(
        keys,
        vec![
            "at_least_one_agent_question".to_string(),
            "at_least_one_user_answer".to_string()
        ]
    );
}

// ---------------------------------------------------------------------------
// Digest
// ---------------------------------------------------------------------------

#[test]
fn the_digest_is_stable_for_identical_transcripts() {
    let transcript = vec![
        turn(1, TurnKind::AgentQuestion, "q"),
        turn(2, TurnKind::UserAnswer, "a"),
    ];
    let (first, len_a) = compute_transcript_digest(&transcript).expect("digest");
    let (second, len_b) = compute_transcript_digest(&transcript).expect("digest again");
    assert_eq!(first, second, "the digest must be deterministic");
    assert_eq!(len_a, len_b);
    assert_eq!(first.len(), 64, "sha256 hex is 64 characters");
}

#[test]
fn the_digest_changes_when_any_content_changes() {
    // The receipt's whole claim is "this is the transcript that was verified".
    // A digest blind to content would let a transcript be edited after
    // finalize without breaking the receipt.
    let base = vec![turn(1, TurnKind::AgentQuestion, "q")];
    let edited = vec![turn(1, TurnKind::AgentQuestion, "q ")];
    let reordered = vec![
        turn(2, TurnKind::UserAnswer, "a"),
        turn(1, TurnKind::AgentQuestion, "q"),
    ];
    let appended = vec![
        turn(1, TurnKind::AgentQuestion, "q"),
        turn(2, TurnKind::UserAnswer, "a"),
    ];

    let (d_base, _) = compute_transcript_digest(&base).expect("digest");
    for (label, other) in [
        ("edited content", edited),
        ("reordered turns", reordered),
        ("appended turn", appended),
    ] {
        let (d_other, _) = compute_transcript_digest(&other).expect("digest");
        assert_ne!(d_base, d_other, "digest ignored: {}", label);
    }
}

#[test]
fn the_digest_reports_the_canonical_byte_length_it_hashed() {
    let transcript = vec![turn(1, TurnKind::AgentQuestion, "q")];
    let (_, len) = compute_transcript_digest(&transcript).expect("digest");
    let expected = serde_json::to_vec(&transcript).expect("canonicalise").len();
    assert_eq!(
        len, expected,
        "the receipt's byte count must be the hashed count"
    );
}

#[test]
fn an_empty_transcript_hashes_the_empty_array_not_nothing() {
    let (digest, len) = compute_transcript_digest(&[]).expect("digest");
    assert_eq!(len, 2, "`[]` is two bytes");
    assert_eq!(digest.len(), 64);
}

// ---------------------------------------------------------------------------
// hex_lower
// ---------------------------------------------------------------------------

#[test]
fn hex_encodes_both_nibbles_in_lower_case() {
    // Hand-rolled per RFC 4648 §8. The nibble order and the low-nibble mask
    // are the two places this can silently be wrong: `0xde` printing as "ed"
    // or "d" still looks like a hex digest.
    assert_eq!(hex_lower(&[]), "");
    assert_eq!(hex_lower(&[0x00]), "00");
    assert_eq!(hex_lower(&[0xff]), "ff");
    assert_eq!(hex_lower(&[0x0f]), "0f");
    assert_eq!(hex_lower(&[0xf0]), "f0");
    assert_eq!(hex_lower(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    assert_eq!(hex_lower(&[0x01, 0x23, 0x45, 0x67]), "01234567");
    assert_eq!(hex_lower(&[0x89, 0xab, 0xcd, 0xef]), "89abcdef");
}

#[test]
fn hex_output_is_two_characters_per_byte() {
    let bytes: Vec<u8> = (0u8..=255).collect();
    let encoded = hex_lower(&bytes);
    assert_eq!(encoded.len(), 512);
    assert!(
        encoded
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "every character must be a lowercase hex digit"
    );
}
