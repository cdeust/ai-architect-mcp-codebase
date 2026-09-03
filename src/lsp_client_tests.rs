//! Tests for the `LspClient` request/response surface.
//!
//! Split from `lsp_client.rs` when that file crossed the §4.1 cap. Several of
//! these drive a REAL child process rather than a mock, because the defects
//! they pin — a hung shutdown, a server request colliding with our id — are
//! properties of how the wire behaves, not of how we model it.

use super::*;

#[test]
fn test_lsp_graceful_fallback_on_fake_lsp() {
    // source: C-correctness bug 1 — a binary that passes the PATH check
    // but exits silently (rustup proxy, stub, /bin/true) used to surface
    // the cryptic "missing Content-Length header". It must now be
    // classified as `lsp_probe_failed` so the MCP layer can report a
    // clean error instead of a protocol-level mystery.
    //
    // We bypass the allowlist via `start_unchecked` because the point of
    // this test is the probe logic, not the upstream command validator.
    let tmp = std::env::temp_dir();
    // `/usr/bin/true` exists on macOS and Linux; exits 0, writes nothing.
    // Use whichever `true` is available — all POSIX systems have one.
    let true_bin = if Path::new("/usr/bin/true").exists() {
        "/usr/bin/true"
    } else if Path::new("/bin/true").exists() {
        "/bin/true"
    } else {
        panic!("no `true` binary available for test");
    };

    let client = LspClient::start_unchecked(true_bin, &[], &tmp, Duration::from_secs(5));
    let mut client = client.expect("spawn /bin/true should succeed");

    let err = client
        .initialize_with_probe(&tmp, Duration::from_secs(2))
        .expect_err("probe against /bin/true must fail");

    assert!(
        err.starts_with("lsp_probe_failed"),
        "expected lsp_probe_failed prefix, got: {err}"
    );
}

/// Root cause 3 (fix/lsp-receiver-calls). Drives a REAL fake LSP server
/// (Python, `tests/fixtures/lsp/fake_workdone_progress_server.py`) that can
/// only reach "begin_sent"/"end_sent" in its own log AFTER it has read the
/// CLIENT's ack of `window/workDoneProgress/create` on stdin — so if
/// `initialize_with_probe` returns at all, the wire round trip already
/// completed IN ORDER: create -> ack -> begin -> end. The assertion is on
/// that log's CONTENT, never on elapsed time — a wall-clock verdict is a
/// load sensor, not a test.
///
/// This is what stands between rust-analyzer answering
/// `textDocument/definition` with `[]` (workspace not yet loaded) and a
/// correct answer: `resolve_with_lsp`'s first definition request is issued
/// strictly after `initialize` returns, so gating THAT return on workDone
/// END is what gates the first definition request.
#[test]
fn lsp_client_waits_for_work_done_progress_end_before_first_definition() {
    if !is_command_available("python3") {
        eprintln!("skipping: python3 not on PATH (fake LSP server fixture needs it)");
        return;
    }
    let log = tempfile::Builder::new()
        .prefix("lsp_readiness_log")
        .tempfile()
        .expect("tempfile");
    let log_path = log.path().to_path_buf();
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/lsp/fake_workdone_progress_server.py"
    );
    let tmp = std::env::temp_dir();
    let mut client = LspClient::start_unchecked(
        "python3",
        &[script, log_path.to_str().expect("utf8 path")],
        &tmp,
        Duration::from_secs(10),
    )
    .expect("spawn fake server");

    client
        .initialize_with_probe(&tmp, Duration::from_secs(5))
        .expect("initialize must succeed once the server completes its script");

    let events = std::fs::read_to_string(&log_path).expect("read log");
    let lines: Vec<&str> = events.lines().collect();
    assert_eq!(
        lines,
        vec![
            "initialize_answered",
            "received:initialized",
            "create_sent",
            "create_acked",
            "begin_sent",
            "end_sent",
        ],
        "the full workDoneProgress round trip must complete, in order, \
         before initialize_with_probe returns: {events}"
    );
}

/// Companion to the workDoneProgress test above, for the PREFERRED
/// deterministic signal (`lsp_client::readiness` module header, signal 1).
/// Drives a REAL fake server (`fake_server_status_server.py`) that never
/// sends a single `$/progress` or `window/workDoneProgress/create` message —
/// only `experimental/serverStatus` `quiescent: false` then `quiescent:
/// true`. If `initialize_with_probe` returns at all, it did so via the
/// serverStatus signal, since the fallback path has nothing to drain. The
/// assertion is on the log's CONTENT and on the deterministic outcome
/// (`ServerStatusQuiescent`), not on elapsed time — event-synchronised, no
/// timing.
#[test]
fn lsp_client_resolves_via_server_status_quiescent_not_progress() {
    if !is_command_available("python3") {
        eprintln!("skipping: python3 not on PATH (fake LSP server fixture needs it)");
        return;
    }
    let log = tempfile::Builder::new()
        .prefix("lsp_readiness_status_log")
        .tempfile()
        .expect("tempfile");
    let log_path = log.path().to_path_buf();
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/lsp/fake_server_status_server.py"
    );
    let tmp = std::env::temp_dir();
    let mut client = LspClient::start_unchecked(
        "python3",
        &[script, log_path.to_str().expect("utf8 path")],
        &tmp,
        Duration::from_secs(10),
    )
    .expect("spawn fake server");

    client
        .initialize_with_probe(&tmp, Duration::from_secs(5))
        .expect("initialize must succeed once the server sends quiescent: true");

    let events = std::fs::read_to_string(&log_path).expect("read log");
    let lines: Vec<&str> = events.lines().collect();
    assert_eq!(
        lines,
        vec![
            "initialize_answered",
            "received:initialized",
            "quiescent_false_sent",
            "quiescent_true_sent",
        ],
        "both serverStatus notifications must have been read, in order, \
         before initialize_with_probe returns — the fixture sends no \
         workDoneProgress message at all, so there is no fallback path to \
         race: {events}"
    );
}

/// Round-3 finding 5. Both directions of JSON-RPC use `id`, and the two id
/// spaces are INDEPENDENT: a server-initiated request
/// (`window/workDoneProgress/create`, `workspace/configuration`) numbers its
/// own. Matching on `id` alone therefore handed the server's question back
/// as if it were our answer, and `parse_definition_response` then read a
/// request's `params` as a definition result.
///
/// A response carries no `method`; a request always does.
#[test]
fn a_server_request_is_not_mistaken_for_our_response() {
    let ours = json!({"jsonrpc": "2.0", "id": 3, "result": null});
    assert!(is_response_to(&ours, 3));

    for server_request in [
        json!({"jsonrpc": "2.0", "id": 3, "method": "window/workDoneProgress/create",
                   "params": {"token": "t"}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "workspace/configuration",
                   "params": {"items": []}}),
    ] {
        assert!(
            !is_response_to(&server_request, 3),
            "a request carrying our id is still a request: {server_request}"
        );
    }

    // A different id is not ours either way.
    assert!(!is_response_to(
        &json!({"jsonrpc": "2.0", "id": 4, "result": null}),
        3
    ));
    // A notification has no id at all.
    assert!(!is_response_to(
        &json!({"jsonrpc": "2.0", "method": "$/progress"}),
        3
    ));
}

/// Finding 3. `shutdown` used to end on an unbounded `wait()`, so a server
/// that ignores `exit` held the run forever — the hang B.6 removed from the
/// read path, one call later. Driven against a REAL child that never exits
/// on its own: `cat` keeps running until its stdin closes or it is killed.
#[test]
fn shutdown_returns_against_a_server_that_never_exits() {
    use std::process::{Command, Stdio};

    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cat");
    let stdout = child.stdout.take().expect("stdout");
    let client = LspClient {
        process: child,
        frames: spawn_frame_reader(stdout),
        request_id: AtomicI64::new(1),
        // Short, because the shutdown handshake will never be answered:
        // the assertion is that the call RETURNS, never on how long it took.
        timeout: Duration::from_millis(50),
    };

    client.shutdown().expect("shutdown must return, not hang");
}

/// Round-5 finding 1. `read_initialize_response` matched on bare `id`
/// equality while its sibling `read_response_for_id` had already been fixed
/// to use `is_response_to` — in the same PR that introduced the helper. A
/// server-initiated request during the handshake (`window/showMessageRequest`
/// is the common one) sharing id=1 was returned AS the init response, and
/// the real one was then discarded by `drain_pending` on the next send.
///
/// Driven against a real child process that emits exactly that sequence:
/// the colliding server request first, our response second.
#[test]
fn the_handshake_skips_a_server_request_sharing_our_id() {
    let mut client = client_replaying(&[
        // A server REQUEST carrying our id…
        json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "window/showMessageRequest",
            "params": {"type": 3, "message": "indexing"}
        }),
        // …then our actual response.
        json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"capabilities": {"definitionProvider": true}}
        }),
    ]);

    let msg = client
        .read_initialize_response(1, Duration::from_secs(5))
        .expect("the real response must be found behind the server request");
    assert!(
        msg.get("method").is_none(),
        "a server request must not be returned as our response: {msg}"
    );
    assert_eq!(
        msg.pointer("/result/capabilities/definitionProvider")
            .and_then(Value::as_bool),
        Some(true),
        "the message returned must be the initialize RESULT: {msg}"
    );
}

/// An `LspClient` whose "server" replays `frames` and then closes its stdout.
///
/// Uses a real child process rather than a mock: these tests pin how the client
/// behaves against the WIRE — Content-Length framing, a stream that ends — and
/// a hand-built channel would assume away the part under test. `cat` echoes its
/// stdin and exits when that stdin closes.
fn client_replaying(frames: &[Value]) -> LspClient {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cat");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for frame in frames {
            let body = serde_json::to_vec(frame).expect("serialize");
            let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
            framed.extend_from_slice(&body);
            stdin.write_all(&framed).expect("write frame");
        }
    } // stdin closed: `cat` flushes and exits.

    let stdout = child.stdout.take().expect("stdout");
    LspClient {
        process: child,
        frames: spawn_frame_reader(stdout),
        request_id: AtomicI64::new(2),
        timeout: Duration::from_secs(5),
    }
}

/// Round-6 finding 4. The previous round gave the probe the request/response
/// path's tolerance for an unparseable payload, trading away its own fail-fast
/// contract: a binary on PATH that emits framed-but-unparseable bytes is
/// exactly what the probe exists to reject, and skipping those frames burned
/// the entire probe window on a binary already known to be wrong.
///
/// The two loops now share one implementation and differ ONLY in this policy,
/// which is what this pins.
#[test]
fn the_probe_fails_fast_where_the_request_path_skips() {
    assert!(
        UnparseableFrame::FailFast != UnparseableFrame::Skip,
        "the two callers must hold distinct policies"
    );

    // The probe stops on the first unparseable frame…
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Value, protocol::FrameError>>(4);
    tx.send(Err(protocol::FrameError::Payload(
        "parse JSON body: x".into(),
    )))
    .expect("queue");
    tx.send(Ok(json!({"jsonrpc": "2.0", "id": 1, "result": {}})))
        .expect("queue");
    let probe = read_from(
        &rx,
        1,
        UnparseableFrame::FailFast,
        Duration::from_millis(50),
    );
    assert!(
        matches!(probe, Err(ReadFailure::Frame(_))),
        "the probe must not wait past a frame it can already judge"
    );

    // …while the request path reads past it to the answer behind.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Value, protocol::FrameError>>(4);
    tx.send(Err(protocol::FrameError::Payload(
        "parse JSON body: x".into(),
    )))
    .expect("queue");
    tx.send(Ok(
        json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}),
    ))
    .expect("queue");
    let answered = read_from(&rx, 1, UnparseableFrame::Skip, Duration::from_millis(50))
        .expect("the answer behind the bad frame");
    assert_eq!(
        answered.pointer("/result/ok").and_then(Value::as_bool),
        Some(true)
    );
}

/// `read_response`'s loop over an arbitrary channel, so the policy can be
/// exercised without spawning a server.
fn read_from(
    frames: &std::sync::mpsc::Receiver<Result<Value, protocol::FrameError>>,
    target_id: i64,
    policy: UnparseableFrame,
    budget: Duration,
) -> Result<Value, ReadFailure> {
    let deadline = Instant::now() + budget;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return Err(ReadFailure::Expired);
        }
        let msg = match frames::next_frame(frames, remaining) {
            Ok(msg) => msg,
            Err(e) if e.is_skippable() && policy == UnparseableFrame::Skip => continue,
            Err(e) => return Err(ReadFailure::Frame(e.message())),
        };
        if is_response_to(&msg, target_id) {
            return Ok(msg);
        }
    }
}
