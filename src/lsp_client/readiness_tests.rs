//! Tests for `lsp_client::readiness`.
//!
//! Split from `readiness.rs` into its own file (§4.1 500-line cap — the plan
//! for issue #282 explicitly asked this file not to grow further) rather than
//! folded into it, mirroring `lsp_client_tests.rs`'s split from `lsp_client.rs`.

use super::*;
use serde_json::json;
use std::sync::mpsc;

fn queue(frames: &[Value]) -> Receiver<Result<Value, FrameError>> {
    let (tx, rx) = mpsc::sync_channel(64);
    for f in frames {
        tx.try_send(Ok(f.clone())).expect("queue frame");
    }
    rx
}

#[test]
fn no_progress_activity_returns_immediately_not_at_the_full_deadline() {
    let rx = queue(&[]);
    let start = Instant::now();
    // A deadline far longer than the onset budget: if this returns only
    // once the WHOLE deadline elapses, the onset budget did nothing.
    let deadline = Instant::now() + Duration::from_secs(30);
    let outcome = wait_for_ready(&rx, |_id| Ok(()), deadline);
    assert_eq!(outcome.readiness, ReadinessOutcome::NoProgressReported);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "a non-reporting server must not be held to the full timeout: {:?}",
        start.elapsed()
    );
}

#[test]
fn create_is_acknowledged_and_begin_then_end_resolves_ready() {
    let mut acked = Vec::new();
    let rx = queue(&[
        json!({"jsonrpc":"2.0","id":7,"method":"window/workDoneProgress/create",
               "params":{"token":"rustAnalyzer/Indexing"}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"rustAnalyzer/Indexing","value":{"kind":"begin","title":"Indexing"}}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"rustAnalyzer/Indexing","value":{"kind":"report","percentage":50}}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"rustAnalyzer/Indexing","value":{"kind":"end"}}}),
    ]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let outcome = wait_for_ready(
        &rx,
        |id| {
            acked.push(id);
            Ok(())
        },
        deadline,
    );
    assert_eq!(outcome.readiness, ReadinessOutcome::AllProgressEnded);
    assert_eq!(acked, vec![7], "the create request must be acknowledged");
}

#[test]
fn two_outstanding_tokens_both_must_end() {
    let rx = queue(&[
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"a","value":{"kind":"begin"}}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"b","value":{"kind":"begin"}}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"a","value":{"kind":"end"}}}),
    ]);
    // Only "a" ended; "b" never does, so the wait must run to its
    // deadline rather than declaring victory early.
    let deadline = Instant::now() + Duration::from_millis(200);
    let outcome = wait_for_ready(&rx, |_id| Ok(()), deadline);
    assert_eq!(outcome.readiness, ReadinessOutcome::DeadlineExpired);
}

/// Regression for the bug this quiet-window redesign fixed: the FIRST
/// version of this detector returned the instant one token's begin/end
/// pair completed. rust-analyzer reports loading as a SEQUENCE of
/// distinct tokens (`Fetching` -> `Building CrateGraph` ->
/// `Roots Scanned` -> ... -> `cachePriming`, measured 2026-09-03), so
/// declaring victory the moment the first pair completed — even though
/// its SECOND token (`create` + begin + end) was already sitting in the
/// channel, ready to read with zero latency. Measured directly against a
/// real fixture: `resolved_count: 0` in 1.3s wall time, because the old
/// code `return`ed the instant `Fetching` ended and never read
/// `Building CrateGraph`'s messages at all — this test proves the second
/// token's `create` gets acknowledged, which only happens if the loop
/// kept running past the first pair's end.
#[test]
fn a_second_tokens_pair_already_queued_is_not_skipped() {
    let mut acked = Vec::new();
    let rx = queue(&[
        json!({"jsonrpc":"2.0","id":1,"method":"window/workDoneProgress/create",
               "params":{"token":"Fetching"}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"Fetching","value":{"kind":"begin"}}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"Fetching","value":{"kind":"end"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"window/workDoneProgress/create",
               "params":{"token":"Building CrateGraph"}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"Building CrateGraph","value":{"kind":"begin"}}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"Building CrateGraph","value":{"kind":"end"}}}),
    ]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let outcome = wait_for_ready(
        &rx,
        |id| {
            acked.push(id);
            Ok(())
        },
        deadline,
    );
    assert_eq!(outcome.readiness, ReadinessOutcome::AllProgressEnded);
    assert_eq!(
        acked,
        vec![1, 2],
        "both tokens' create requests must be acknowledged — a loop \
         that returns the instant the first token's pair ends would \
         never read the second token's messages at all"
    );
}

/// Signal 1 (module header) must win even while a workDoneProgress token
/// is still technically outstanding — `quiescent: true` is the server's
/// own authoritative answer, not another vote to reconcile against the
/// drain-and-debounce fallback.
#[test]
fn quiescent_status_short_circuits_before_progress_has_settled() {
    let rx = queue(&[
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"still-going","value":{"kind":"begin"}}}),
        json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
               "params":{"health":"ok","quiescent":true}}),
    ]);
    let deadline = Instant::now() + Duration::from_secs(30);
    let start = Instant::now();
    let outcome = wait_for_ready(&rx, |_id| Ok(()), deadline);
    assert_eq!(outcome.readiness, ReadinessOutcome::ServerReportedQuiescent);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "quiescent must resolve immediately, not wait on the still-open \
         token or the 30s deadline: {:?}",
        start.elapsed()
    );
}

/// `quiescent: false` is activity, not readiness — it must not be
/// mistaken for the `true` case. With nothing else ever arriving, the
/// wait still gives up as `NoProgressReported` (there is nothing to be
/// ready FOR), but only once the CALLER's deadline passes, not the
/// short workDoneProgress onset window — see the next test for why that
/// distinction is load-bearing.
#[test]
fn quiescent_false_falls_through_to_the_progress_fallback() {
    let rx = queue(
        &[json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
               "params":{"health":"ok","quiescent":false}})],
    );
    let deadline = Instant::now() + Duration::from_millis(700);
    let outcome = wait_for_ready(&rx, |_id| Ok(()), deadline);
    assert_eq!(outcome.readiness, ReadinessOutcome::NoProgressReported);
}

/// Regression: a `quiescent: false` used to be a no-op in `apply` — it
/// matched neither `server_workdone_progress_create` nor
/// `progress_kind`, so `ever_began` stayed false and `checkpoint` kept
/// returning the FIXED 500ms `onset_deadline` from construction. A
/// `quiescent: true` arriving in the (event-driven) mocked channel
/// AFTER that window — as it legitimately can under real load, where
/// rust-analyzer's own indexing is what quiescence is waiting on — was
/// therefore missed: the loop gave up with `NoProgressReported` and the
/// deterministic signal was never read. Measured directly:
/// `cargo test --lib` under ~495 tests of contention flaked
/// `lsp_client::tests::lsp_client_resolves_via_server_status_quiescent_not_progress`
/// this exact way. `note_server_status_activity` fixes it: any
/// serverStatus message, `true` or `false`, is proof the server offers
/// signal 1 and the onset heuristic no longer applies.
#[test]
fn a_quiescent_false_extends_the_wait_past_the_onset_window() {
    let rx = queue(&[
        json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
               "params":{"health":"ok","quiescent":false}}),
        // If `note_server_status_activity` did not extend the
        // checkpoint, the loop would have already given up (onset
        // window is 500ms) before this message is even queued at
        // "delivery time" in a real channel — here it is available
        // immediately, so a pre-fix implementation would still miss it
        // by exiting on its FIRST spin without ever reading it, since
        // its checkpoint decision does not depend on what is in the
        // channel.
        json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
               "params":{"health":"ok","quiescent":true}}),
    ]);
    // A deadline well past the 500ms onset window: the ONLY way this
    // resolves as quiescent is if the false->true pair both got read.
    let deadline = Instant::now() + Duration::from_secs(2);
    let outcome = wait_for_ready(&rx, |_id| Ok(()), deadline);
    assert_eq!(outcome.readiness, ReadinessOutcome::ServerReportedQuiescent);
}

#[test]
fn a_numeric_token_and_a_string_token_are_the_same_key_space() {
    let rx = queue(&[
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":42,"value":{"kind":"begin"}}}),
        json!({"jsonrpc":"2.0","method":"$/progress",
               "params":{"token":"42","value":{"kind":"end"}}}),
    ]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let outcome = wait_for_ready(&rx, |_id| Ok(()), deadline);
    assert_eq!(
        outcome.readiness,
        ReadinessOutcome::AllProgressEnded,
        "a token begun as a number and ended as its string form is one token"
    );
}

/// Issue #282 §1.2: `health: "error"` must short-circuit the wait via the
/// SAME `quiescent: true` path as a healthy server — sonde B (issue
/// investigation) showed both arrive on the one message — and the
/// server's own message text must survive to the caller, who turns it
/// into `lsp_workspace_load_failed: <message>` before ever issuing a
/// `textDocument/definition` request.
#[test]
fn error_health_with_quiescent_true_reports_error_and_the_server_message() {
    let rx = queue(
        &[json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
               "params":{"health":"error","quiescent":true,
                         "message":"Failed to load workspaces."}})],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let outcome = wait_for_ready(&rx, |_id| Ok(()), deadline);
    assert_eq!(outcome.readiness, ReadinessOutcome::ServerReportedQuiescent);
    assert_eq!(outcome.level, ServerHealthLevel::Error);
    assert_eq!(
        outcome.message.as_deref(),
        Some("Failed to load workspaces.")
    );
}

/// Sonde D (issue investigation): a `warning` health can precede an
/// `error` on the same wait. The LAST health observed must win, not the
/// first — a caller gating on `Error` must see it even though a
/// `warning` arrived earlier in the same handshake.
#[test]
fn warning_then_error_reports_error_the_last_one_seen() {
    let rx = queue(&[
        json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
               "params":{"health":"warning","quiescent":false,
                         "message":"Failed to discover workspace"}}),
        json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
               "params":{"health":"error","quiescent":true,
                         "message":"Failed to load workspaces."}}),
    ]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let outcome = wait_for_ready(&rx, |_id| Ok(()), deadline);
    assert_eq!(outcome.level, ServerHealthLevel::Error);
    assert_eq!(
        outcome.message.as_deref(),
        Some("Failed to load workspaces."),
        "the LAST health observed must win, not the first"
    );
}
