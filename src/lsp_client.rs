// lsp_client — lightweight LSP client over stdio JSON-RPC 2.0.
//
// Spawns a language server process, sends initialize/didOpen/definition
// requests, and parses responses. Uses Content-Length header framing
// per the LSP specification (not newline-delimited).
//
// source: LSP Specification 3.17 — https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/

use serde_json::{json, Value};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

mod commands;
mod frames;
mod protocol;
mod uri;

pub use commands::{
    detect_lsp_command, is_command_available, validate_lsp_command, LSP_COMMAND_ALLOWLIST,
};
use frames::{drain_pending, next_frame, spawn_frame_reader};
use protocol::FrameError;
pub(crate) use protocol::{is_lsp_timeout, LSP_TIMEOUT_PREFIX};
pub use uri::{file_uri_to_path, path_to_file_uri};

use protocol::{
    classify_probe_err, parse_definition_response, validate_probe_response, write_lsp_message,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// How many times `shutdown` re-checks whether the server exited on its own
/// before killing it, and how long it waits between checks — 200 ms in total.
///
/// The budget only has to exceed a COOPERATIVE server's flush-and-exit after it
/// receives `exit`; a server still running past it is not going to stop, and
/// waiting longer only delays the kill. It is deliberately orders of magnitude
/// below the request timeout, because this is a teardown path.
///
/// source: provisional heuristic. Calibrate against a measured server that
/// exits cleanly but takes longer than this ceiling — its flush time is the
/// number to raise this to; a torn cache after shutdown is the symptom that
/// would show it is too low.
const SHUTDOWN_GRACE_POLLS: u32 = 20;
const SHUTDOWN_GRACE_STEP: Duration = Duration::from_millis(10);

pub struct LspClient {
    process: Child,
    frames: Receiver<Result<Value, FrameError>>,
    request_id: AtomicI64,
    timeout: Duration,
}

pub struct DefinitionResult {
    pub uri: String,
    pub start_line: u64,
    #[allow(dead_code)] // available for column-precise matching in future
    pub start_col: u64,
}

pub struct LspResolutionResult {
    pub resolved_count: u64,
    pub failed_count: u64,
    pub skipped_count: u64,
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// LspClient implementation
// ---------------------------------------------------------------------------

impl LspClient {
    /// Spawn an LSP server process and return a client handle.
    ///
    /// SECURITY: `command` must be an allowlisted bare binary name — see
    /// `validate_lsp_command`. Callers that accept user input MUST validate
    /// before calling this; `start` double-checks in case it's bypassed.
    pub fn start(
        command: &str,
        args: &[&str],
        workspace_root: &Path,
        timeout: Duration,
    ) -> Result<Self, String> {
        // Defense-in-depth — reject anything not on the allowlist, even if
        // the caller forgot to validate upstream.
        validate_lsp_command(command)?;
        Self::start_unchecked(command, args, workspace_root, timeout)
    }

    /// Internal spawn that skips the allowlist. Only reachable from `start()`
    /// (which validates) and from unit tests that need to exercise the probe
    /// logic against a fake binary living outside the allowlist.
    ///
    /// `pub(crate)` — NOT reachable from MCP argument plumbing. All external
    /// call paths still go through `start()` → `validate_lsp_command`.
    #[doc(hidden)]
    pub(crate) fn start_unchecked(
        command: &str,
        args: &[&str],
        workspace_root: &Path,
        timeout: Duration,
    ) -> Result<Self, String> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn {command}: {e}"))?;

        let stdout = child.stdout.take().ok_or("failed to capture LSP stdout")?;
        let frames = spawn_frame_reader(stdout);

        Ok(LspClient {
            process: child,
            frames,
            request_id: AtomicI64::new(1),
            timeout,
        })
    }

    /// Send `initialize` and `initialized` to the LSP server.
    ///
    /// This is the **probing handshake**: the first response is read under a
    /// short `probe_timeout` (separate from the tool's main timeout) so that
    /// binaries on PATH that aren't real LSP servers — rustup proxies,
    /// shell stubs, `/bin/true` — fail fast with `lsp_probe_failed` instead of
    /// the cryptic `missing Content-Length header` the old code surfaced.
    ///
    /// source: C-correctness bug 1 — graceful-fallback claim was false when
    /// the binary existed but didn't speak LSP.
    pub fn initialize(&mut self, workspace_root: &Path) -> Result<(), String> {
        self.initialize_with_probe(workspace_root, Duration::from_secs(2))
    }

    /// Initialize with an explicit probe timeout for the first response.
    /// Exposed so callers (and tests) can tune the probe window.
    pub fn initialize_with_probe(
        &mut self,
        workspace_root: &Path,
        probe_timeout: Duration,
    ) -> Result<(), String> {
        // source: M2 fix — percent-encode the path so spaces, unicode, and
        // URL-reserved chars in workspace paths don't produce a malformed URI.
        let root_uri = path_to_file_uri(workspace_root);
        let id = self.next_id();

        let init_req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "definition": { "dynamicRegistration": false }
                    }
                },
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": workspace_root.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                }]
            }
        });

        self.send_request(&init_req).map_err(classify_probe_err)?;

        // Read the first response under probe_timeout. Any failure here —
        // EOF, timeout, parse error, missing jsonrpc field — is reclassified
        // as `lsp_probe_failed` so callers can distinguish "not on PATH"
        // from "on PATH but not an LSP server".
        let resp = self.read_initialize_response(id, probe_timeout)?;
        validate_probe_response(&resp)?;

        // Send initialized notification (no id, no response expected)
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        self.send_notification(&notif).map_err(classify_probe_err)?;

        Ok(())
    }

    fn read_initialize_response(
        &mut self,
        id: i64,
        probe_timeout: Duration,
    ) -> Result<Value, String> {
        self.read_response(id, probe_timeout, UnparseableFrame::FailFast)
            .map_err(|e| match e {
                ReadFailure::Expired => format!(
                    "lsp_probe_failed: no response within probe timeout ({}ms)",
                    probe_timeout.as_millis()
                ),
                ReadFailure::Frame(message) => classify_probe_err(message),
            })
    }

    /// Notify the server about an open document.
    pub fn did_open(
        &mut self,
        file_uri: &str,
        language_id: &str,
        text: &str,
    ) -> Result<(), String> {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": file_uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text
                }
            }
        });
        self.send_notification(&notif)
    }

    /// Query textDocument/definition at a specific position.
    pub fn get_definition(
        &mut self,
        file_uri: &str,
        line: u64,
        col: u64,
    ) -> Result<Option<DefinitionResult>, String> {
        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": file_uri },
                "position": { "line": line, "character": col }
            }
        });

        self.send_request(&req)?;
        let resp = self.read_response_for_id(id)?;

        parse_definition_response(&resp)
    }

    /// Gracefully shut down the LSP server.
    pub fn shutdown(mut self) -> Result<(), String> {
        let id = self.next_id();
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": "shutdown", "params": null });
        if self.send_request(&req).is_ok() {
            let _ = self.read_response_for_id(id);
        }
        let notif = json!({ "jsonrpc": "2.0", "method": "exit", "params": null });
        let _ = self.send_notification(&notif);
        // BOUNDED, with a grace period. `wait()` alone blocks forever on a
        // server that ignores `exit` — the same hang B.6 removed from the read
        // path, one call later — but a single `try_wait` fired immediately
        // after the write SIGKILLs a well-behaved server that merely needs a
        // few milliseconds to flush, risking a torn cache. Poll briefly, then
        // fall back to kill; `wait` after `kill` returns promptly.
        //
        // source: provisional heuristic. The budget only needs to exceed a
        // cooperative server's flush-and-exit, which is milliseconds for the
        // servers this drives; it is deliberately far below the request
        // timeout, because a server that has not exited by then is not going to.
        for _ in 0..SHUTDOWN_GRACE_POLLS {
            if matches!(self.process.try_wait(), Ok(Some(_))) {
                return Ok(());
            }
            std::thread::sleep(SHUTDOWN_GRACE_STEP);
        }
        let _ = self.process.kill();
        let _ = self.process.wait();
        Ok(())
    }

    // -- private helpers --

    fn next_id(&self) -> i64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    fn send_request(&mut self, msg: &Value) -> Result<(), String> {
        // Emptying the queue here NARROWS the deadlock window; it does not
        // close it. The cycle is: a full queue stops the reader thread, which
        // stops draining the server's stdout, which fills the server's stdout
        // pipe, which stops the server reading our stdin, which hangs the write
        // below. This drain runs ONCE, and `write_lsp_message` then makes two
        // separate blocking `write_all` calls with nothing draining between
        // them — so a payload large enough for a chatty server to refill the
        // channel mid-write can still hang. `did_open` carries a whole file,
        // which is where that is reachable.
        //
        // TRACKED, NOT CLOSED. Closing it needs the drain to run DURING the
        // write (chunked writes, or non-blocking pipe IO) — a change of shape
        // rather than of wording, and deliberately out of scope here.
        drain_pending(&self.frames);
        let bytes = serde_json::to_vec(msg).map_err(|e| format!("serialize request: {e}"))?;
        let stdin = self.process.stdin.as_mut().ok_or("LSP stdin unavailable")?;
        write_lsp_message(stdin, &bytes)
    }

    fn send_notification(&mut self, msg: &Value) -> Result<(), String> {
        self.send_request(msg)
    }

    /// Read messages until one is a response to `target_id`.
    ///
    /// Discards notifications, server-initiated requests and other responses
    /// along the way. `LSP_TIMEOUT_PREFIX` on expiry, so a timed-out request
    /// classifies as one everywhere.
    fn read_response_for_id(&mut self, target_id: i64) -> Result<Value, String> {
        self.read_response(target_id, self.timeout, UnparseableFrame::Skip)
            .map_err(|e| match e {
                ReadFailure::Expired => {
                    format!("{LSP_TIMEOUT_PREFIX} waiting for response id={target_id}")
                }
                ReadFailure::Frame(message) => message,
            })
    }

    /// The ONE deadline/skip/match loop, shared by the probe and the
    /// request/response path.
    ///
    /// These were two independently-maintained copies, and that is precisely
    /// why the bare-`id` match survived two review rounds after `is_response_to`
    /// was added to only one of them. They differ in exactly two things — the
    /// error wording, which the callers apply, and what an unparseable frame
    /// means, which `policy` names — so those are the only two parameters.
    fn read_response(
        &self,
        target_id: i64,
        budget: Duration,
        policy: UnparseableFrame,
    ) -> Result<Value, ReadFailure> {
        let deadline = Instant::now() + budget;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                return Err(ReadFailure::Expired);
            }
            let msg = match next_frame(&self.frames, remaining) {
                Ok(msg) => msg,
                Err(e) if e.is_skippable() && policy == UnparseableFrame::Skip => continue,
                Err(e) => return Err(ReadFailure::Frame(e.message())),
            };
            if is_response_to(&msg, target_id) {
                return Ok(msg);
            }
            // A notification, a server-initiated request, or another response.
            // A server request sharing our id (`window/showMessageRequest`
            // during the handshake) must NOT be taken for our answer.
        }
    }
}

/// What a read loop does with a frame whose payload would not parse but whose
/// stream is still aligned.
///
/// The two callers need OPPOSITE behaviour, so unifying them on one policy is
/// what the previous round got wrong:
#[derive(Clone, Copy, PartialEq, Eq)]
enum UnparseableFrame {
    /// Skip it and keep waiting. A request/response exchange must survive one
    /// malformed notification from a server that is otherwise answering.
    Skip,
    /// Fail immediately. The PROBE is deciding whether this binary speaks LSP
    /// at all, and a framed-but-unparseable first frame IS that answer — so
    /// tolerating it burns the whole probe window on a binary already known to
    /// be wrong, against the probe's own fail-fast contract.
    FailFast,
}

/// Why `read_response` stopped, kept separate from its wording because the two
/// callers report the same cause differently.
#[derive(Debug)]
enum ReadFailure {
    /// The budget ran out.
    Expired,
    /// A frame error the policy did not absorb.
    Frame(String),
}

/// True when `msg` is a RESPONSE to `target_id`, as opposed to a server-
/// initiated REQUEST that happens to carry the same id.
///
/// Both directions of JSON-RPC use `id`, and the two id spaces are independent:
/// a server request (`window/workDoneProgress/create`,
/// `workspace/configuration`) numbers its own, so matching on `id` alone
/// returned the server's question to us as if it were our answer. A response
/// carries no `method`; a request always does. source: JSON-RPC 2.0 §5 and LSP
/// 3.17 §Base Protocol.
fn is_response_to(msg: &Value, target_id: i64) -> bool {
    msg.get("method").is_none() && msg.get("id").and_then(Value::as_i64) == Some(target_id)
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[cfg(test)]
#[path = "lsp_client_tests.rs"]
mod tests;
