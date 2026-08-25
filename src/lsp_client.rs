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
use frames::{next_frame, spawn_frame_reader};
pub(crate) use protocol::{is_lsp_timeout, LSP_TIMEOUT_PREFIX};
pub use uri::{file_uri_to_path, path_to_file_uri};

use protocol::{
    classify_probe_err, parse_definition_response, validate_probe_response, write_lsp_message,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct LspClient {
    process: Child,
    frames: Receiver<Result<Value, String>>,
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
        let deadline = Instant::now() + probe_timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                return Err(format!(
                    "lsp_probe_failed: no response within probe timeout ({}ms)",
                    probe_timeout.as_millis()
                ));
            }
            let msg = next_frame(&self.frames, remaining).map_err(classify_probe_err)?;
            if msg.get("id").and_then(|v| v.as_i64()) == Some(id) {
                return Ok(msg);
            }
            // notifications / other ids — keep reading within the window.
        }
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
        let _ = self.process.wait();
        Ok(())
    }

    // -- private helpers --

    fn next_id(&self) -> i64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    fn send_request(&mut self, msg: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(msg).map_err(|e| format!("serialize request: {e}"))?;
        let stdin = self.process.stdin.as_mut().ok_or("LSP stdin unavailable")?;
        write_lsp_message(stdin, &bytes)
    }

    fn send_notification(&mut self, msg: &Value) -> Result<(), String> {
        self.send_request(msg)
    }

    /// Read messages until we find one with the matching id.
    /// Discards notifications and other responses along the way.
    fn read_response_for_id(&mut self, target_id: i64) -> Result<Value, String> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                return Err(format!(
                    "{LSP_TIMEOUT_PREFIX} waiting for response id={target_id}"
                ));
            }
            let msg = next_frame(&self.frames, remaining)?;
            // Check if this is our response
            if let Some(id) = msg.get("id") {
                if id.as_i64() == Some(target_id) {
                    return Ok(msg);
                }
            }
            // Otherwise it's a notification or different response — skip
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[cfg(test)]
mod tests {
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
}
