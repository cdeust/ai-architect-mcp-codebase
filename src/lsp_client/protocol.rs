// lsp_client::protocol — the JSON-RPC wire below `LspClient`.
//
// Split from `lsp_client` when that file crossed the §4.1 500-line cap, along
// the seam between "drive a language server" and "frame and read its
// messages". Content-Length framing and response parsing are pure functions
// over bytes and JSON, so they are testable without spawning a process.
//
// source: LSP Specification 3.17 §Base Protocol.

use super::DefinitionResult;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Content-Length framing — LSP wire protocol
// source: LSP spec §Base Protocol
// ---------------------------------------------------------------------------

/// Prefix every timeout this module raises carries, so a caller classifies a
/// timed-out request by an EXACT match on a value this module owns rather than
/// by sniffing for the word "timeout" anywhere in an error string. A server
/// error that merely mentions the word — a symbol named `timeout`, a message
/// quoting one — is not a timeout, and counting it as one silently moved a
/// real failure into the `skipped` bucket.
pub(crate) const LSP_TIMEOUT_PREFIX: &str = "lsp_timeout:";

/// True when `e` is a timeout this module raised.
pub(crate) fn is_lsp_timeout(e: &str) -> bool {
    e.starts_with(LSP_TIMEOUT_PREFIX)
}

pub(super) fn write_lsp_message(
    stdin: &mut std::process::ChildStdin,
    msg: &[u8],
) -> Result<(), String> {
    let header = format!("Content-Length: {}\r\n\r\n", msg.len());
    stdin
        .write_all(header.as_bytes())
        .map_err(|e| format!("write header: {e}"))?;
    stdin
        .write_all(msg)
        .map_err(|e| format!("write body: {e}"))?;
    stdin.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

/// Reads ONE framed message, blocking until it has one or the stream ends.
///
/// This is deliberately unbounded: it runs on the reader thread
/// [`spawn_frame_reader`] owns, and the caller's deadline is enforced on the
/// channel instead. It used to carry an `Instant`-based deadline checked at the
/// top of the header loop, which could not bound anything — `read_line` and
/// `read_exact` block in the kernel, so a server that sent a partial header and
/// then stopped never reached the check again and the whole indexer hung on it.
/// A timeout that is only consulted between blocking calls is not a timeout.
/// Why a frame could not be produced, and — the part that matters — whether the
/// stream is still usable afterwards.
pub(super) enum FrameError {
    /// The body was read in full and the stream is still byte-aligned; only the
    /// PAYLOAD was unusable. The next frame can be read normally.
    Payload(String),
    /// Framing or IO failed, so the stream position is unknown and no later
    /// frame can be trusted.
    Fatal(String),
}

impl FrameError {
    pub(super) fn message(self) -> String {
        match self {
            FrameError::Payload(m) | FrameError::Fatal(m) => m,
        }
    }
}

pub(super) fn read_lsp_message(
    reader: &mut BufReader<std::process::ChildStdout>,
) -> Result<Value, FrameError> {
    // Read headers until empty line. If the server closes stdout (EOF)
    // before we see a Content-Length header, callers must be able to tell
    // that apart from a real protocol message — we surface it via
    // `eof_before_header` so the probe path can map to `lsp_probe_failed`.
    let mut content_length: Option<usize> = None;
    let mut saw_any_byte = false;
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| FrameError::Fatal(format!("read header line: {e}")))?;
        if n == 0 {
            // EOF — child closed stdout.
            if saw_any_byte {
                return Err(FrameError::Fatal(
                    "eof_before_body: partial header then EOF".to_string(),
                ));
            }
            return Err(FrameError::Fatal(
                "eof_before_header: child stdout closed without LSP framing".to_string(),
            ));
        }
        saw_any_byte = true;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse::<usize>().ok();
        }
    }

    let len = content_length
        .ok_or_else(|| FrameError::Fatal("missing Content-Length header".to_string()))?;
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .map_err(|e| FrameError::Fatal(format!("read body ({len} bytes): {e}")))?;

    // The body was consumed in full, so the stream is still aligned: a
    // malformed PAYLOAD costs this one frame, not the connection.
    serde_json::from_slice(&body).map_err(|e| FrameError::Payload(format!("parse JSON body: {e}")))
}

/// How many parsed frames may sit unread before the reader thread blocks.
///
/// The bound is the point of it. An UNBOUNDED channel replaces the OS pipe's
/// backpressure with none at all: a notification-heavy server — rust-analyzer
/// emits `$/progress` continuously while it indexes — would have its frames
/// eagerly read AND JSON-parsed into a queue nobody drains between requests,
/// so memory grows with the server's chatter rather than with our demand. With
/// a bound, a full queue simply stops the reader in `read_line`, which is
/// exactly where the kernel used to stop it.
///
/// Timeout semantics are untouched: the deadline lives on the receiving end,
/// and a blocked sender cannot extend it.
///
/// source: provisional heuristic. It must exceed the notification burst a
/// server emits between two of our requests (a handful for the servers this
/// drives) and stay small enough that the queue is not itself the leak. 64 sits
/// well above the former and far below the latter; calibrate against a measured
/// server that stalls on it.
const FRAME_QUEUE_DEPTH: usize = 64;

/// Moves `reader` onto a thread that pushes every frame down a bounded channel,
/// and returns the receiving end.
///
/// This is what makes the caller's timeout real. The blocking framing reads
/// live on a thread whose fate does not matter — when the child is killed on
/// `LspClient::drop`, its stdout closes, `read_lsp_message` returns the EOF
/// error, the loop ends and the thread exits. The caller waits on the channel
/// with a deadline it can actually enforce.
pub(super) fn spawn_frame_reader(
    stdout: std::process::ChildStdout,
) -> Receiver<Result<Value, String>> {
    let (tx, rx) = mpsc::sync_channel(FRAME_QUEUE_DEPTH);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            // Only a FRAMING or IO failure ends the run. A malformed payload
            // leaves the stream byte-aligned, and one bad notification must not
            // kill LSP resolution for every request that follows it — which is
            // what exiting on any error did, mislabelling every later request
            // `eof_before_header`.
            let frame = read_lsp_message(&mut reader);
            let stop = matches!(frame, Err(FrameError::Fatal(_)));
            let sent = tx.send(frame.map_err(FrameError::message));
            // A closed receiver means the client was dropped; stop.
            if sent.is_err() || stop {
                return;
            }
        }
    });
    rx
}

/// Waits up to `timeout` for the next frame.
///
/// A `RecvTimeoutError::Timeout` becomes the module's timeout sentinel, so it
/// classifies identically to one raised anywhere else here. `Disconnected`
/// means the reader thread ended — it has already sent whatever error ended it,
/// so anything after that is EOF.
pub(super) fn next_frame(
    frames: &Receiver<Result<Value, String>>,
    timeout: Duration,
) -> Result<Value, String> {
    match frames.recv_timeout(timeout) {
        Ok(frame) => frame,
        Err(RecvTimeoutError::Timeout) => {
            Err(format!("{LSP_TIMEOUT_PREFIX} no frame within {timeout:?}"))
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err("eof_before_header: child stdout closed without LSP framing".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Probe classification — correctness bug 1
// ---------------------------------------------------------------------------

/// Rewrites any low-level framing/parse/timeout error raised during the
/// handshake into an `lsp_probe_failed:` error so the resolver and MCP
/// response layer can report a distinct reason code. If the message is
/// already an `lsp_probe_failed:` string, it is returned unchanged.
pub(super) fn classify_probe_err(e: String) -> String {
    if e.starts_with("lsp_probe_failed") {
        return e;
    }
    let reason = if e.contains("eof_before_header") {
        "found on PATH but didn't respond as an LSP server (stdout closed immediately; likely a stub, proxy, or non-LSP binary)"
    } else if e.contains("eof_before_body") {
        "found on PATH but dropped the connection mid-header (partial LSP framing then EOF)"
    } else if e.contains("missing Content-Length") {
        "found on PATH but sent non-LSP output (no Content-Length header)"
    } else if is_lsp_timeout(&e) || e.contains("no response within probe timeout") {
        "found on PATH but didn't respond within the probe window (not an LSP server or hung)"
    } else if e.contains("parse JSON body") {
        "found on PATH but sent non-JSON-RPC bytes as the first frame"
    } else {
        return format!("lsp_probe_failed: {e}");
    };
    format!("lsp_probe_failed: {reason} — underlying: {e}")
}

/// Validates the first message claims to be JSON-RPC 2.0 with a `result` or
/// `error` field. Anything else is a probe failure.
pub(super) fn validate_probe_response(resp: &Value) -> Result<(), String> {
    let jsonrpc = resp.get("jsonrpc").and_then(|v| v.as_str());
    if jsonrpc != Some("2.0") {
        return Err(format!(
            "lsp_probe_failed: first response missing jsonrpc=\"2.0\" field (got {:?})",
            jsonrpc
        ));
    }
    if resp.get("result").is_none() && resp.get("error").is_none() {
        return Err(
            "lsp_probe_failed: first response has neither `result` nor `error`".to_string(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

pub(super) fn parse_definition_response(resp: &Value) -> Result<Option<DefinitionResult>, String> {
    if let Some(err) = resp.get("error") {
        return Err(format!("LSP error: {err}"));
    }
    let result = match resp.get("result") {
        Some(Value::Null) | None => return Ok(None),
        Some(r) => r,
    };

    // LSP returns either a single Location, an array of Locations,
    // or an array of LocationLinks. Handle all cases.
    let location = if result.is_array() {
        let arr = result.as_array().unwrap();
        if arr.is_empty() {
            return Ok(None);
        }
        &arr[0]
    } else if result.is_object() {
        result
    } else {
        return Ok(None);
    };

    // LocationLink has targetUri + targetRange; Location has uri + range
    let uri = location
        .get("targetUri")
        .or_else(|| location.get("uri"))
        .and_then(|v| v.as_str());
    let range = location
        .get("targetRange")
        .or_else(|| location.get("range"));

    match (uri, range) {
        (Some(u), Some(r)) => {
            let start = r.get("start").ok_or("missing range.start")?;
            let line = start.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
            let col = start.get("character").and_then(|v| v.as_u64()).unwrap_or(0);
            Ok(Some(DefinitionResult {
                uri: u.to_string(),
                start_line: line,
                start_col: col,
            }))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_definition_single_location() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "uri": "file:///src/main.rs",
                "range": {
                    "start": { "line": 10, "character": 4 },
                    "end": { "line": 10, "character": 20 }
                }
            }
        });
        let def = parse_definition_response(&resp).unwrap().unwrap();
        assert_eq!(def.uri, "file:///src/main.rs");
        assert_eq!(def.start_line, 10);
        assert_eq!(def.start_col, 4);
    }

    #[test]
    fn test_parse_definition_array() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": [{
                "uri": "file:///src/lib.rs",
                "range": {
                    "start": { "line": 5, "character": 0 },
                    "end": { "line": 5, "character": 10 }
                }
            }]
        });
        let def = parse_definition_response(&resp).unwrap().unwrap();
        assert_eq!(def.uri, "file:///src/lib.rs");
        assert_eq!(def.start_line, 5);
    }

    #[test]
    fn test_parse_definition_null_result() {
        let resp = json!({ "jsonrpc": "2.0", "id": 3, "result": null });
        assert!(parse_definition_response(&resp).unwrap().is_none());
    }

    #[test]
    fn test_parse_definition_empty_array() {
        let resp = json!({ "jsonrpc": "2.0", "id": 4, "result": [] });
        assert!(parse_definition_response(&resp).unwrap().is_none());
    }

    #[test]
    fn test_parse_definition_location_link() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "result": [{
                "targetUri": "file:///src/graph_store.rs",
                "targetRange": {
                    "start": { "line": 20, "character": 0 },
                    "end": { "line": 30, "character": 1 }
                },
                "targetSelectionRange": {
                    "start": { "line": 20, "character": 11 },
                    "end": { "line": 20, "character": 21 }
                }
            }]
        });
        let def = parse_definition_response(&resp).unwrap().unwrap();
        assert_eq!(def.uri, "file:///src/graph_store.rs");
        assert_eq!(def.start_line, 20);
    }

    #[test]
    fn test_parse_definition_error() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "error": { "code": -32600, "message": "invalid request" }
        });
        assert!(parse_definition_response(&resp).is_err());
    }

    #[test]
    fn test_classify_probe_err_eof() {
        let classified = classify_probe_err(
            "eof_before_header: child stdout closed without LSP framing".to_string(),
        );
        assert!(classified.starts_with("lsp_probe_failed:"));
        assert!(classified.contains("stub, proxy, or non-LSP binary"));
    }

    #[test]
    fn test_classify_probe_err_passthrough() {
        let already = "lsp_probe_failed: already classified".to_string();
        assert_eq!(classify_probe_err(already.clone()), already);
    }

    #[test]
    fn test_validate_probe_response_rejects_non_jsonrpc() {
        let bad = json!({ "id": 1, "result": {} });
        assert!(validate_probe_response(&bad).is_err());
        let bad2 = json!({ "jsonrpc": "2.0", "id": 1 });
        assert!(validate_probe_response(&bad2).is_err());
        let good = json!({ "jsonrpc": "2.0", "id": 1, "result": {} });
        assert!(validate_probe_response(&good).is_ok());
    }
}

#[cfg(test)]
mod frame_bound_tests {
    use super::*;
    use std::sync::mpsc;

    /// B.6. The bound has to hold when the server sends a partial header and
    /// then simply stops — the shape a hung language server presents.
    ///
    /// Before this change the deadline was an `Instant` compared at the top of
    /// the header loop, and `read_line` blocks in the kernel, so control never
    /// returned to the check: `read_lsp_message` sat there for as long as the
    /// child lived and took the indexer with it. A timeout consulted only
    /// between blocking calls is not a timeout.
    ///
    /// The assertion is on the RETURNED VALUE, never on elapsed time: this must
    /// not become a test whose verdict depends on machine load. A channel that
    /// never receives is what the pre-change code produced, and this test would
    /// hang there rather than fail — which is why the fix moves the blocking
    /// read off the caller's thread instead of tightening the check.
    #[test]
    fn next_frame_times_out_when_no_frame_ever_arrives() {
        // A sender held open with nothing sent models a server that wrote a
        // partial header and stopped: the reader thread is still blocked, so
        // no frame is ever pushed.
        let (_tx, rx) = mpsc::channel::<Result<Value, String>>();
        let err = next_frame(&rx, Duration::from_millis(50))
            .expect_err("a frame that never arrives must not block forever");
        assert!(
            is_lsp_timeout(&err),
            "must classify as this module's timeout: {err}"
        );
    }

    /// A reader thread that has ended (child gone) reports EOF, not a timeout —
    /// the two are different answers and the probe classifier maps them to
    /// different reason codes.
    #[test]
    fn next_frame_reports_eof_when_the_reader_thread_is_gone() {
        let (tx, rx) = mpsc::channel::<Result<Value, String>>();
        drop(tx);
        let err = next_frame(&rx, Duration::from_millis(50)).expect_err("no frame");
        assert!(
            !is_lsp_timeout(&err),
            "a dead reader is not a timeout: {err}"
        );
        assert!(err.contains("eof_before_header"), "{err}");
    }

    /// The queue is BOUNDED, so a chatty server cannot grow it without limit —
    /// a full queue stops the reader thread in its blocking read, which is
    /// where the OS pipe used to stop it before the thread existed. Filling it
    /// must not affect what a waiting caller sees.
    #[test]
    fn a_full_queue_blocks_the_producer_without_disturbing_the_consumer() {
        let (tx, rx) = mpsc::sync_channel::<Result<Value, String>>(FRAME_QUEUE_DEPTH);
        for i in 0..FRAME_QUEUE_DEPTH {
            tx.try_send(Ok(serde_json::json!({ "id": i })))
                .expect("the queue accepts up to its depth");
        }
        assert!(
            tx.try_send(Ok(serde_json::json!({"id": "overflow"})))
                .is_err(),
            "past its depth the queue must refuse, which is what blocks a \
             real sender instead of growing memory"
        );
        // The consumer still reads in order, and draining makes room again.
        let first = next_frame(&rx, Duration::from_millis(50)).expect("queued frame");
        assert_eq!(first.get("id").and_then(|v| v.as_i64()), Some(0));
        tx.try_send(Ok(serde_json::json!({"id": "now fits"})))
            .expect("a drained slot is reusable");
    }

    /// Frames the thread already pushed are delivered without waiting.
    #[test]
    fn next_frame_delivers_a_queued_frame() {
        let (tx, rx) = mpsc::channel::<Result<Value, String>>();
        tx.send(Ok(serde_json::json!({"id": 1}))).expect("send");
        let msg = next_frame(&rx, Duration::from_millis(50)).expect("queued frame");
        assert_eq!(msg.get("id").and_then(|v| v.as_i64()), Some(1));
    }

    /// Re-review finding 3. The reader thread exited on ANY error, including a
    /// JSON-parse failure on a body that had been read IN FULL — the stream is
    /// still byte-aligned there, and the next frame is perfectly readable. One
    /// malformed notification therefore killed LSP resolution for the rest of
    /// the run, with every later request mislabelled `eof_before_header`.
    ///
    /// The classification is what the thread's loop keys on, so it is the
    /// classification this pins.
    #[test]
    fn a_malformed_payload_is_recoverable_but_broken_framing_is_not() {
        let payload = read_frame_error(b"Content-Length: 3\r\n\r\nnot");
        assert!(
            matches!(payload, Some(FrameError::Payload(_))),
            "a fully-consumed body that will not parse costs one frame only"
        );

        let no_header = read_frame_error(b"garbage without framing\r\n\r\n");
        assert!(
            matches!(no_header, Some(FrameError::Fatal(_))),
            "missing Content-Length leaves the stream position unknown"
        );

        let truncated = read_frame_error(b"Content-Length: 99\r\n\r\nshort");
        assert!(
            matches!(truncated, Some(FrameError::Fatal(_))),
            "a body shorter than its declared length desynchronises the stream"
        );
    }

    /// Drives `read_lsp_message`'s framing over a byte slice by way of a real
    /// child process, since its parameter is a `ChildStdout` reader.
    fn read_frame_error(bytes: &[u8]) -> Option<FrameError> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn cat");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(bytes)
            .expect("write");
        let stdout = child.stdout.take().expect("stdout");
        let mut reader = BufReader::new(stdout);
        let out = read_lsp_message(&mut reader).err();
        let _ = child.wait();
        out
    }
}
