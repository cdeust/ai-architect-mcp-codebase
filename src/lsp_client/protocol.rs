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
#[derive(Debug)]
pub(super) enum FrameError {
    /// The body was read in full and the stream is still byte-aligned; only the
    /// PAYLOAD was unusable. The next frame can be read normally.
    Payload(String),
    /// Framing or IO failed, so the stream position is unknown and no later
    /// frame can be trusted.
    Fatal(String),
    /// No frame arrived inside the caller's deadline. The stream is fine; this
    /// call simply ran out of time.
    Timeout(String),
}

impl FrameError {
    pub(super) fn message(self) -> String {
        match self {
            FrameError::Payload(m) | FrameError::Fatal(m) | FrameError::Timeout(m) => m,
        }
    }

    /// True when the frame was unusable but the STREAM was not: the caller may
    /// skip it and keep waiting inside its own deadline.
    pub(super) fn is_skippable(&self) -> bool {
        matches!(self, FrameError::Payload(_))
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

    let uri = preferred_uri(location);
    let range = preferred_range(location);

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

/// `location`'s URI: LocationLink has `targetUri`, Location has only `uri`.
fn preferred_uri(location: &Value) -> Option<&str> {
    location
        .get("targetUri")
        .or_else(|| location.get("uri"))
        .and_then(|v| v.as_str())
}

/// `location`'s preferred range, precise before loose.
///
/// LocationLink has `targetRange` + (optionally) `targetSelectionRange`;
/// Location has only `range`.
///
/// source: LSP 3.17 §textDocument/definition. `targetSelectionRange` is
/// "the range that should be selected and revealed when this link is being
/// followed, e.g. the name of a function" — the PRECISE identifier-name
/// span. `targetRange`/`range` is the loose declaration span ("including
/// e.g. comments"). Preferring the precise range when present is what lets
/// `find_node_at_position` match on an exact line instead of a fuzzy
/// nearby-line scan (fabricated `total -> total` self-edge, PR #267
/// follow-up).
fn preferred_range(location: &Value) -> Option<&Value> {
    location
        .get("targetSelectionRange")
        .or_else(|| location.get("targetRange"))
        .or_else(|| location.get("range"))
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
        assert_eq!(
            def.start_col, 11,
            "targetSelectionRange (the precise identifier span) must be \
             preferred over the loose targetRange"
        );
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
