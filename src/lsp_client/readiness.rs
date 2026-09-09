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
use super::health::{parse_server_status, ServerHealth, ServerHealthLevel, ServerStatus};
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

/// Why `wait_for_ready` returned. `pub` (not `pub(super)`): it rides on
/// `ServerHealth::readiness`, which is a field of a `pub` struct reachable
/// through `LspResolutionResult` — a private-in-public-interface error
/// otherwise (E0446).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessOutcome {
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

impl ReadinessOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReadinessOutcome::ServerReportedQuiescent => "server_reported_quiescent",
            ReadinessOutcome::NoProgressReported => "no_progress_reported",
            ReadinessOutcome::AllProgressEnded => "all_progress_ended",
            ReadinessOutcome::DeadlineExpired => "deadline_expired",
        }
    }
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

/// Tracks the LAST `experimental/serverStatus.health`/`.message` observed
/// during one `wait_for_ready` call, independent of the progress/quiescent
/// bookkeeping `ProgressState` owns — a server can report `health` on every
/// serverStatus message, not only the final `quiescent: true` one (sonde D,
/// `health.rs` module header: `warning` arrives before `error` on the same
/// wait). "Last wins" is deliberate: it is the server's most recent verdict,
/// not its first.
struct HealthTrack {
    level: ServerHealthLevel,
    message: Option<String>,
}

impl HealthTrack {
    fn new() -> Self {
        HealthTrack {
            level: ServerHealthLevel::Unknown,
            message: None,
        }
    }

    fn record(&mut self, status: &ServerStatus) {
        self.level = status.health;
        self.message = status.message.clone();
    }

    fn finish(self, readiness: ReadinessOutcome) -> ServerHealth {
        ServerHealth {
            level: self.level,
            message: self.message,
            readiness,
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
/// Returns the server's health as last observed during the wait (`Unknown`
/// with no message for a server that never sends `experimental/serverStatus`
/// at all), paired with why the wait ended — see `ServerHealth`.
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
) -> ServerHealth {
    let mut state = ProgressState::new();
    let mut health = HealthTrack::new();
    loop {
        let now = Instant::now();
        let checkpoint = state.checkpoint(deadline);
        if now >= checkpoint {
            return health.finish(state.give_up());
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
            Err(_) => return health.finish(state.give_up()),
        };

        // Signal 1 wins outright, even over a still-outstanding progress
        // token — it is the server's own authoritative answer, not another
        // heuristic to reconcile with the drain below.
        match parse_server_status(&msg) {
            Some(status) => {
                health.record(&status);
                if status.quiescent {
                    return health.finish(ReadinessOutcome::ServerReportedQuiescent);
                }
                // `quiescent: false` is proof the server WILL eventually
                // report `true` — stop applying the workDoneProgress
                // onset/quiet-window heuristics, which exist only for a
                // server that might report nothing at all (see
                // `server_status_seen`).
                state.note_server_status_activity();
            }
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
    let health = wait_for_ready(
        frames,
        |id| {
            let ack = json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null });
            let bytes = serde_json::to_vec(&ack).map_err(|e| format!("serialize ack: {e}"))?;
            let stdin = process.stdin.as_mut().ok_or("LSP stdin unavailable")?;
            write_lsp_message(stdin, &bytes)
        },
        deadline,
    );
    client.server_health = health;
}

enum ProgressKind {
    Begin,
    Report,
    End,
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
#[path = "readiness_tests.rs"]
mod tests;
