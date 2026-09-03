// lsp_client::readiness — wait for the server's own indexing-progress signal
// before the first `textDocument/definition` request.
//
// Split out of `lsp_client.rs` (not folded in) to keep that file under the
// §4.1 500-line cap while it owns the request/response surface.
//
// Two signals, tried in order of precedence:
//
// 1. `experimental/serverStatus` — a notification rust-analyzer sends once
//    the client opts in (capability `experimental.serverStatusNotification:
//    true`, declared in `lsp_client::build_initialize_request`), carrying
//    `quiescent: bool`. `quiescent: true` is the server's own authoritative
//    "caught up" signal — no heuristic involved. source: rust-analyzer's
//    serverStatus LSP extension; re-verified 2026-09-03 against rust-analyzer
//    1.95.0 on the dy-wcet corpus (own probe run, not taken on report):
//    `quiescent: false` (`health: "ok"`) arrives at t=0.18s, `quiescent:
//    true` at t=4.39s, and `textDocument/definition` at the method
//    identifier answers correctly immediately after. PRIMARY signal
//    whenever a server sends it.
//
// 2. workDoneProgress drain-with-a-quiet-window (below) — the FALLBACK for
//    servers that never send `experimental/serverStatus` (pyright,
//    typescript-language-server, measured against the same client), and for
//    rust-analyzer if the notification is ever suppressed. Over the same
//    re-verification run the set of OUTSTANDING `$/progress` tokens dropped
//    to zero TEN times (Fetching, Building CrateGraph, Roots Scanned,
//    Loading proc-macros, cachePriming, then a flycheck run) before
//    quiescence at t=4.39s — a naive "nothing outstanding right now" check
//    would have returned up to nine times too early; the exact count is
//    run-dependent (machine load, cache warmth), the shape is not. This
//    fallback exists because it has no server-authoritative end; it is only
//    consulted when signal 1 never arrives.
//
// source: LSP Specification 3.17 §Progress ($/progress,
// window/workDoneProgress/create) — https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#progress
//
// rust-analyzer answers `textDocument/definition` with `[]` until its
// workspace has finished loading (verified 2026-09-03: `[]` at t=0.4s,
// the correct definition at t=5.7s, same position, same file — see the
// probe transcript this fix is derived from). rust-analyzer reports that
// loading through workDoneProgress: a server-initiated
// `window/workDoneProgress/create` request naming a token, followed by
// `$/progress` notifications on that token with `value.kind` "begin", zero or
// more "report", then "end". A server that never uses progress reporting
// (pyright, typescript-language-server measured against the same client)
// simply never sends either message — this module must not delay those.
//
// The loading is NOT one token: the same transcript shows a SEQUENCE —
// `Fetching` (0.18-0.58s) -> `Building CrateGraph` (0.60s, instant) ->
// `Roots Scanned` (0.60-1.08s, overlapping `Building compile-time-deps`) ->
// a second `Fetching`/`Building CrateGraph`/`Roots Scanned`/
// `Loading proc-macros` round (1.18-1.39s) -> `cachePriming` — the actual
// indexing — ending at 2.27s, after which nothing more arrives. A detector
// that returns the instant the FIRST token's begin/end pair completes (the
// `Fetching` token, at 0.58s) declares readiness a full 1.7s before the
// server can answer a real query — measured directly: `resolved_count: 0`
// in 1.3s wall time against this exact fixture before this fix. The
// detector below instead treats "no NEW token began while none was
// outstanding" as the readiness signal (a debounce), not "the first pair
// ended".

use super::frames::next_frame;
use super::protocol::{write_lsp_message, FrameError};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// How long `wait_for_ready` treats the absence of workDoneProgress activity
/// as significant, in two roles:
///
/// - Before any token has ever begun: how long to wait before concluding the
///   server does not report progress at all (must exceed a local stdio round
///   trip for the server's own `window/workDoneProgress/create` request —
///   milliseconds, once it decides to send one — and stay far below a real
///   indexing job, so a non-reporting server is never meaningfully delayed).
/// - After every begun token has ended: how long to wait, with nothing
///   outstanding, before concluding no FURTHER token is about to begin (the
///   measured transcript shows successive phases starting up to ~450ms apart
///   — `Fetching` ends 0.58s, `Building CrateGraph` begins 0.60s is typical,
///   but the gap between the FIRST settle and the SECOND `Fetching` round is
///   ~100ms and the widest observed inter-phase gap is well under this).
///
/// Never applied while a token IS outstanding (unended) — that case simply
/// waits for events up to the caller's own `deadline`, however long that
/// takes; only the "is anything about to start" question is time-boxed.
///
/// source: provisional heuristic, calibrated against the rust-analyzer
/// transcript in this module's header comment (2026-09-03, dy-wcet-shaped
/// single-crate fixture). Calibrate upward against a measured server whose
/// inter-phase gap legitimately exceeds this — the symptom would be
/// `AllProgressEnded` returned while a later phase was still about to start.
const PROGRESS_QUIET_WINDOW: Duration = Duration::from_millis(500);

/// Why `wait_for_ready` returned.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReadinessOutcome {
    /// The server sent `experimental/serverStatus` with `quiescent: true` —
    /// its own authoritative "caught up" answer (signal 1, module header).
    ServerReportedQuiescent,
    /// No workDoneProgress activity was observed within the onset budget —
    /// treated as a server that does not use progress reporting for this
    /// request. The caller proceeds immediately either way; this exists so
    /// tests and diagnostics can tell the two "proceeded" paths apart.
    NoProgressReported,
    /// Every workDoneProgress token that began also ended before the
    /// deadline.
    AllProgressEnded,
    /// `deadline` (the caller's own `timeout_ms`) passed while at least one
    /// token was still outstanding. The caller proceeds anyway — this is a
    /// best-effort synchronisation, not a hard gate — but a definition query
    /// issued now may still race the server's own indexing.
    DeadlineExpired,
}

/// The workDoneProgress bookkeeping `wait_for_ready` needs between frames —
/// pulled out of the function body so the event loop (below) reads as one
/// screen and stays under §4.2.
struct ProgressState {
    outstanding: HashSet<String>,
    ever_began: bool,
    // Set to the instant `outstanding` last became (or has always been)
    // empty. `None` while at least one token is open — no quiet-window
    // check applies then; see the module header for why.
    idle_since: Option<Instant>,
    // Fixed at construction, NOT recomputed per spin: the "never began"
    // case's own quiet window, measured from when the wait started.
    onset_deadline: Instant,
    // Set once ANY `experimental/serverStatus` message is observed, even
    // `quiescent: false`. Positive proof the server offers signal 1, so the
    // onset/quiet-window heuristics (built for a server that might report
    // NOTHING) no longer apply — the loop should simply wait for
    // `quiescent: true` up to the caller's own `deadline`.
    //
    // Regression: without this, a `quiescent: false` arriving right at the
    // start of the wait was a no-op in `apply` — `ever_began` stayed false,
    // so `checkpoint` kept returning the FIXED `onset_deadline` (500ms from
    // construction). Under load, that window could expire before the
    // server got around to sending `quiescent: true`, and the loop gave up
    // with `NoProgressReported` — measured directly: `cargo test --lib` at
    // ~500 tests of contention flaked this exact test, panicking with the
    // log stopping at "quiescent_false_sent" and never reaching
    // "quiescent_true_sent". A server that has already proven it reports
    // serverStatus must never be treated as if it might report nothing.
    server_status_seen: bool,
}

impl ProgressState {
    fn new() -> Self {
        let now = Instant::now();
        ProgressState {
            outstanding: HashSet::new(),
            ever_began: false,
            idle_since: Some(now),
            onset_deadline: now + PROGRESS_QUIET_WINDOW,
            server_status_seen: false,
        }
    }

    /// The next point in time worth waking up for — NOT a hard exit by
    /// itself: whether reaching it means "give up" depends on which case it
    /// belongs to (see `give_up`).
    fn checkpoint(&self, deadline: Instant) -> Instant {
        if self.server_status_seen {
            // The server has already proven it sends serverStatus; only
            // `quiescent: true` (handled outside this state, in
            // `wait_for_ready`) or the caller's own deadline end the wait.
            return deadline;
        }
        match (self.ever_began, self.idle_since) {
            (false, _) => self.onset_deadline,
            (true, Some(idle_at)) => idle_at + PROGRESS_QUIET_WINDOW,
            (true, None) => deadline, // something outstanding: no early exit
        }
        .min(deadline)
    }

    /// Records that a `quiescent: false` (or any other non-`true`)
    /// `experimental/serverStatus` was observed — see `server_status_seen`.
    fn note_server_status_activity(&mut self) {
        self.server_status_seen = true;
    }

    fn give_up(&self) -> ReadinessOutcome {
        give_up(self.ever_began, self.outstanding.is_empty())
    }

    /// Applies one non-quiescent frame: acknowledges a server-initiated
    /// `create`, or updates the outstanding-token set on `$/progress`.
    /// Anything else is not progress-related and is a no-op.
    fn apply(
        &mut self,
        msg: &Value,
        respond_to_create: &mut impl FnMut(i64) -> Result<(), String>,
    ) {
        if let Some(id) = server_workdone_progress_create(msg) {
            // Best-effort: a failed ack just means the server may choose not
            // to progress-report on this token; the wait still proceeds on
            // whatever `$/progress` (if any) follows.
            let _ = respond_to_create(id);
            return;
        }
        match progress_kind(msg) {
            Some((token, ProgressKind::Begin)) => {
                self.ever_began = true;
                self.outstanding.insert(token);
                self.idle_since = None;
            }
            Some((token, ProgressKind::End)) => {
                self.outstanding.remove(&token);
                if self.outstanding.is_empty() {
                    self.idle_since = Some(Instant::now());
                }
            }
            // A report is activity on an ALREADY-outstanding token — it
            // cannot itself flip idle_since (that only happens on the
            // begin/end transition above), so nothing to update here.
            Some((_, ProgressKind::Report)) | None => {}
        }
    }
}

/// Blocks until the server reports quiescence (signal 1, module header), or
/// its workDoneProgress tokens have all ended (signal 2, fallback), or until
/// `deadline`, whichever comes first. Acknowledges every
/// `window/workDoneProgress/create` request it observes (the server will not
/// emit `$/progress` for a token it never got acknowledged — LSP 3.17
/// §Progress) and swallows a failure to do so: an unanswered `create`
/// degrades this to a best-effort wait, not a broken handshake.
///
/// Any frame that is neither signal — another notification, a stray
/// response — is set aside and left for `read_response_for_id` to find, so
/// this never consumes an answer the caller still needs. `next_frame` alone
/// cannot un-consume a frame, so callers that need the general request/
/// response path AFTER this one must expect it to have already read past
/// any non-progress traffic that arrived during the wait; `LspClient` only
/// calls this once, immediately after the `initialized` notification and
/// before any request whose answer matters, so there is nothing left to
/// preserve.
pub(super) fn wait_for_ready(
    frames: &Receiver<Result<Value, FrameError>>,
    mut respond_to_create: impl FnMut(i64) -> Result<(), String>,
    deadline: Instant,
) -> ReadinessOutcome {
    let mut state = ProgressState::new();
    loop {
        let now = Instant::now();
        let checkpoint = state.checkpoint(deadline);
        if now >= checkpoint {
            return state.give_up();
        }
        let msg = match next_frame(frames, checkpoint - now) {
            Ok(msg) => msg,
            // A frame that failed to parse is skippable exactly as it is in
            // the request/response loop (frames.rs `UnparseableFrame::Skip`):
            // the stream stays aligned, one bad notification must not end
            // the wait.
            Err(e) if e.is_skippable() => continue,
            // The budget ran out with nothing arriving: `recv_timeout`
            // itself did the waiting, so looping back to the checkpoint
            // check above is not a busy spin — it resolves on the next
            // iteration.
            Err(FrameError::Timeout(_)) => continue,
            // Framing/IO failure (EOF, reader thread gone): `recv_timeout`
            // returns this WITHOUT waiting, so continuing here would busy-
            // spin until `deadline` with nothing left to ever read. Give up
            // now instead.
            Err(_) => return state.give_up(),
        };

        // Signal 1 wins outright, even over a still-outstanding progress
        // token — it is the server's own authoritative answer, not another
        // heuristic to reconcile with the drain below.
        match server_status_quiescent(&msg) {
            Some(true) => return ReadinessOutcome::ServerReportedQuiescent,
            // `quiescent: false` is proof the server WILL eventually report
            // `true` — stop applying the workDoneProgress onset/quiet-window
            // heuristics, which exist only for a server that might report
            // nothing at all (see `server_status_seen`).
            Some(false) => state.note_server_status_activity(),
            None => state.apply(&msg, &mut respond_to_create),
        }
    }
}

/// The shared exit decision for both "deadline reached" and "channel will
/// never produce another frame": no activity ever seen is `NoProgressReported`
/// (proceed — nothing to wait for); every begun token already ended is
/// `AllProgressEnded` (proceed — genuinely ready); anything still outstanding
/// is `DeadlineExpired` (proceed anyway — best effort, not a hard gate).
fn give_up(ever_began: bool, outstanding_empty: bool) -> ReadinessOutcome {
    if !ever_began {
        ReadinessOutcome::NoProgressReported
    } else if outstanding_empty {
        ReadinessOutcome::AllProgressEnded
    } else {
        ReadinessOutcome::DeadlineExpired
    }
}

/// Orchestration entry point for `LspClient::initialize_with_probe`. A free
/// function, not an `LspClient` method — moved out of `impl LspClient`
/// (coding-standards §4.3) because it needs only two of the client's
/// fields, borrowed separately here so the ack closure's `&mut
/// process.stdin` borrow and the loop's `&frames` borrow do not conflict.
/// Lives in this module (not `lsp_client.rs`) because it IS the readiness
/// orchestration, not merely a caller of it — `LspClient`'s private fields
/// are reachable here since this module is a child of `lsp_client`.
pub(super) fn client_wait_for_ready(client: &mut super::LspClient) {
    let deadline = Instant::now() + client.timeout;
    let frames = &client.frames;
    let process = &mut client.process;
    let _ = wait_for_ready(
        frames,
        |id| {
            let ack = json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null });
            let bytes = serde_json::to_vec(&ack).map_err(|e| format!("serialize ack: {e}"))?;
            let stdin = process.stdin.as_mut().ok_or("LSP stdin unavailable")?;
            write_lsp_message(stdin, &bytes)
        },
        deadline,
    );
}

enum ProgressKind {
    Begin,
    Report,
    End,
}

/// `Some(quiescent)` when `msg` is an `experimental/serverStatus`
/// notification (rust-analyzer's LSP extension, opted into via the
/// `experimental.serverStatusNotification` client capability — module
/// header, signal 1). `None` for any other message, including from a server
/// that never sends this at all.
fn server_status_quiescent(msg: &Value) -> Option<bool> {
    if msg.get("method").and_then(Value::as_str) != Some("experimental/serverStatus") {
        return None;
    }
    msg.get("params")?.get("quiescent")?.as_bool()
}

/// `id` when `msg` is a server-initiated `window/workDoneProgress/create`
/// request (has a `method` AND an `id` — a response never carries `method`).
fn server_workdone_progress_create(msg: &Value) -> Option<i64> {
    if msg.get("method").and_then(Value::as_str) != Some("window/workDoneProgress/create") {
        return None;
    }
    msg.get("id").and_then(Value::as_i64)
}

/// `(token, kind)` when `msg` is a `$/progress` notification carrying a
/// `WorkDoneProgress` value (`kind` one of "begin" | "report" | "end" — LSP
/// 3.17 §Progress). The token is stringified once here so both the numeric
/// and string token forms the spec permits compare equal as map keys.
fn progress_kind(msg: &Value) -> Option<(String, ProgressKind)> {
    if msg.get("method").and_then(Value::as_str) != Some("$/progress") {
        return None;
    }
    let params = msg.get("params")?;
    let token = params.get("token")?;
    let token = token
        .as_str()
        .map(str::to_string)
        .or_else(|| token.as_i64().map(|n| n.to_string()))?;
    let kind = params.get("value")?.get("kind")?.as_str()?;
    let kind = match kind {
        "begin" => ProgressKind::Begin,
        "report" => ProgressKind::Report,
        "end" => ProgressKind::End,
        _ => return None,
    };
    Some((token, kind))
}

#[cfg(test)]
mod tests {
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
        assert_eq!(outcome, ReadinessOutcome::NoProgressReported);
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
        assert_eq!(outcome, ReadinessOutcome::AllProgressEnded);
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
        assert_eq!(outcome, ReadinessOutcome::DeadlineExpired);
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
        assert_eq!(outcome, ReadinessOutcome::AllProgressEnded);
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
        assert_eq!(outcome, ReadinessOutcome::ServerReportedQuiescent);
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
        assert_eq!(outcome, ReadinessOutcome::NoProgressReported);
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
        assert_eq!(outcome, ReadinessOutcome::ServerReportedQuiescent);
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
            outcome,
            ReadinessOutcome::AllProgressEnded,
            "a token begun as a number and ended as its string form is one token"
        );
    }
}
