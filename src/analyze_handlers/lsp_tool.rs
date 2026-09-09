//! Stage 3b-v2 — `lsp_resolve` (standalone LSP-enhanced resolution) and its
//! shared language-detection helper.
//!
//! Split out of `analyze_handlers.rs` (coding-standards.md §4.1, 500-line file
//! cap) per `tasks/plan-issues-282-283-284.md` §0.1/§1.3, which named this
//! exact file as near-cap and prescribed this move in advance. Follows the
//! `analyze_handlers/lsp_outcome.rs` submodule pattern already established in
//! this directory. Behaviour-preserving: no logic changed, only relocated.

use crate::graph_store;
use crate::lsp_client;
use crate::lsp_resolver;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use super::lsp_outcome;

pub(crate) fn run_lsp_resolve(arguments: &Value) -> Value {
    match do_lsp_resolve(arguments) {
        Ok(v) => v,
        Err(msg) => {
            // Distinguish specific failure reasons so callers can act on them.
            if msg.contains("lsp_command_not_allowed") {
                // source: C3 fix — surface the reason code plus the allowlist
                // so the caller knows which commands are accepted.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_command_not_allowed",
                    "message": msg,
                    "allowed": lsp_client::LSP_COMMAND_ALLOWLIST,
                })
            } else if msg.contains("lsp_not_found") {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_not_found",
                    "message": msg
                })
            } else if msg.contains("lsp_probe_failed") {
                // source: C-correctness bug 1 — binary on PATH but doesn't
                // speak LSP (rustup proxy, stub script, /bin/true, ...).
                // Distinct from lsp_not_found so callers can act on it.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_probe_failed",
                    "message": msg
                })
            } else if msg.contains("lsp_workspace_load_failed") {
                // Issue #282: the server itself reported it could not load
                // the analyzed project (nested under a parent Cargo
                // workspace that does not list it as a member is the
                // measured case) — distinct so a caller can act on the
                // cargo-level remedy the message names, not retry a
                // language-server invocation that will fail identically.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_workspace_load_failed",
                    "message": msg
                })
            } else {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_resolve_failed",
                    "message": msg
                })
            }
        }
    }
}

/// The caller-supplied half of an `lsp_resolve` call, validated once.
/// A parameter object (§4.4): the four settings are resolved together and
/// used together by `do_lsp_resolve`.
struct LspResolveRequest<'a> {
    graph_path: &'a Path,
    codebase_path: &'a Path,
    language: String,
    lsp_command: Option<&'a str>,
    timeout: std::time::Duration,
}

impl<'a> LspResolveRequest<'a> {
    fn prepare(args: &'a serde_json::Map<String, Value>) -> Result<Self, String> {
        let graph_str = args
            .get("graph_path")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'graph_path'")?;
        let codebase_str = args
            .get("codebase_path")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'codebase_path'")?;
        let language = args
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("auto");
        let graph_path = Path::new(graph_str);
        if !graph_path.exists() {
            return Err(format!("graph_path does not exist: {graph_str}"));
        }
        let codebase_path = Path::new(codebase_str);
        if !codebase_path.exists() {
            return Err(format!("codebase_path does not exist: {codebase_str}"));
        }
        Ok(LspResolveRequest {
            graph_path,
            codebase_path,
            // Auto-detect language from codebase if needed.
            language: if language == "auto" {
                detect_dominant_language(codebase_path)
            } else {
                language.to_string()
            },
            lsp_command: args.get("lsp_command").and_then(|v| v.as_str()),
            timeout: std::time::Duration::from_millis(
                args.get("timeout_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(30000),
            ),
        })
    }
}

/// Issue #282: the same `state`/`server_health` vocabulary `analyze_codebase`'s
/// `lsp_status` carries (`lsp_outcome`), so a caller of this standalone tool
/// gets the same "ran but resolved nothing" signal.
fn lsp_resolve_envelope(result: &lsp_client::LspResolutionResult) -> Value {
    json!({
        "stage": 3,
        "status": "ok",
        "tool": "lsp_resolve",
        "resolved_count": result.resolved_count,
        "failed_count": result.failed_count,
        "skipped_count": result.skipped_count,
        "elapsed_ms": result.elapsed_ms,
        "state": lsp_outcome::completed_state(result),
        "server_health": lsp_outcome::server_health_json(&result.server_health),
    })
}

pub(crate) fn do_lsp_resolve(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let req = LspResolveRequest::prepare(args)?;
    let store = graph_store::GraphStore::open_or_create(req.graph_path)?;
    let result = lsp_resolver::resolve_with_lsp(
        &store,
        req.codebase_path,
        &req.language,
        req.lsp_command,
        req.timeout,
    )?;
    Ok(lsp_resolve_envelope(&result))
}

/// Detect the dominant language from file extensions in a codebase.
pub(crate) fn detect_dominant_language(path: &Path) -> String {
    let mut rs_count = 0u32;
    let mut py_count = 0u32;
    let mut ts_count = 0u32;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            match p.extension().and_then(|e| e.to_str()) {
                Some("rs") => rs_count += 1,
                Some("py") => py_count += 1,
                Some("ts") | Some("tsx") => ts_count += 1,
                _ => {}
            }
        }
    }

    if rs_count >= py_count && rs_count >= ts_count {
        "rust".to_string()
    } else if py_count >= ts_count {
        "python".to_string()
    } else {
        "typescript".to_string()
    }
}
