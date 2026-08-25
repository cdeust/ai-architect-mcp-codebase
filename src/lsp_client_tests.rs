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
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn frame(v: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(v).expect("serialize");
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        out
    }

    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cat");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        // A server REQUEST carrying our id, then our actual response.
        stdin
            .write_all(&frame(&json!({
                "jsonrpc": "2.0", "id": 1,
                "method": "window/showMessageRequest",
                "params": {"type": 3, "message": "indexing"}
            })))
            .expect("write request");
        stdin
            .write_all(&frame(&json!({
                "jsonrpc": "2.0", "id": 1,
                "result": {"capabilities": {"definitionProvider": true}}
            })))
            .expect("write response");
    } // stdin closed: `cat` flushes and exits.

    let stdout = child.stdout.take().expect("stdout");
    let mut client = LspClient {
        process: child,
        frames: spawn_frame_reader(stdout),
        request_id: AtomicI64::new(2),
        timeout: Duration::from_secs(5),
    };

    let msg = client
        .read_initialize_response(1, Duration::from_secs(5))
        .expect("the real response must be found behind the server request");
    assert!(
        msg.get("method").is_none(),
        "a server request must not be returned as our response: {msg}"
    );
    assert!(
        msg.pointer("/result/capabilities/definitionProvider")
            .and_then(Value::as_bool)
            == Some(true),
        "the message returned must be the initialize RESULT: {msg}"
    );
}
