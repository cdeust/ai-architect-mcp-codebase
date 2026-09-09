// lsp_client::health — the server's own opinion of whether it loaded the
// workspace, read from `experimental/serverStatus`'s `health` and `message`
// fields (rust-analyzer's LSP extension) alongside the `quiescent` signal
// `readiness` already tracks.
//
// Split out of `readiness.rs` (not folded in) to keep that file under the
// §4.1 500-line cap, and because parsing the wire shape is a distinct
// concern from the debounce/quiet-window bookkeeping `readiness` owns.
//
// source: rust-analyzer's serverStatus LSP extension; `health` values
// (`"ok"`, `"warning"`, `"error"`) and `message` verified 2026-09-03 against
// rust-analyzer 1.95.0 — see `readiness.rs`'s module header for the
// `quiescent` timing evidence and issue #282's investigation for the
// `health` values themselves:
//   - crate seul: `{health:"ok", quiescent:false}` then `{health:"ok",
//     quiescent:true}`.
//   - crate sous un `[workspace] members = []` parent qui ne le liste pas:
//     `{health:"error", quiescent:true, message:"Failed to load
//     workspaces."}` — the server itself reports the failure `readiness`
//     alone discards (`quiescent: true` short-circuits on `true` regardless
//     of `health`).
//   - rootUri = workspace parent, fichier enfant non listé:
//     `{health:"warning", quiescent:true, message:"Failed to read Cargo
//     metadata with dependencies…"}` then a second `warning` — resolution
//     answers `[]` for every site despite `quiescent: true`, so `warning`
//     must stay visible to a caller even though it does not fail the phase
//     (§1.2 decision: only `error` fails outright).

use super::readiness::ReadinessOutcome;
use serde_json::Value;

/// The server's own report of whether it successfully loaded the analyzed
/// project, read from `experimental/serverStatus.health`. `Unknown` covers
/// both "no serverStatus message was ever observed" (pyright,
/// typescript-language-server never send this extension) and "no LSP
/// session was opened at all" (nothing to resolve, so `resolve_with_lsp`
/// never spawned a server) — in neither case is there a server opinion to
/// report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerHealthLevel {
    Ok,
    Warning,
    Error,
    Unknown,
}

impl ServerHealthLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerHealthLevel::Ok => "ok",
            ServerHealthLevel::Warning => "warning",
            ServerHealthLevel::Error => "error",
            ServerHealthLevel::Unknown => "unknown",
        }
    }
}

/// The server's health as last observed during `initialize`'s readiness
/// wait, paired with why the wait itself ended (`readiness`). "Last
/// observed" matters: sonde D (module header, `readiness.rs`) showed
/// `warning` arrive before `error` on the SAME wait — the caller needs the
/// final verdict, not the first one.
#[derive(Debug, Clone)]
pub struct ServerHealth {
    pub level: ServerHealthLevel,
    pub message: Option<String>,
    pub readiness: ReadinessOutcome,
}

impl ServerHealth {
    /// No `experimental/serverStatus` message was ever observed, and no
    /// readiness wait occurred at all — used for the case where
    /// `resolve_with_lsp` never spawns a client because there was nothing to
    /// resolve. Distinct from a server that ran and stayed silent about
    /// health (which also lands on `Unknown`, but carries a real
    /// `ReadinessOutcome` from an actual wait).
    pub fn not_probed() -> Self {
        ServerHealth {
            level: ServerHealthLevel::Unknown,
            message: None,
            readiness: ReadinessOutcome::NoProgressReported,
        }
    }
}

/// One `experimental/serverStatus` notification, parsed. `quiescent` is
/// always present when this parses `Some` — rust-analyzer sends it on every
/// serverStatus message, and `readiness`'s debounce logic depends on that
/// (module header: "quiescent: false is activity, not readiness").
#[derive(Debug, Clone)]
pub(crate) struct ServerStatus {
    pub health: ServerHealthLevel,
    pub quiescent: bool,
    pub message: Option<String>,
}

/// `Some` when `msg` is an `experimental/serverStatus` notification; `None`
/// for any other message, including from a server that never sends this
/// extension at all. An unrecognized `health` string (a future rust-analyzer
/// value this module doesn't yet know) maps to `Unknown` rather than being
/// treated as a parse failure — the `quiescent` signal this replaces
/// (`server_status_quiescent`, pre-fix) tolerated the same forward
/// compatibility.
pub(crate) fn parse_server_status(msg: &Value) -> Option<ServerStatus> {
    if msg.get("method").and_then(Value::as_str) != Some("experimental/serverStatus") {
        return None;
    }
    let params = msg.get("params")?;
    let quiescent = params.get("quiescent")?.as_bool()?;
    let health = match params.get("health").and_then(Value::as_str) {
        Some("ok") => ServerHealthLevel::Ok,
        Some("warning") => ServerHealthLevel::Warning,
        Some("error") => ServerHealthLevel::Error,
        _ => ServerHealthLevel::Unknown,
    };
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(ServerStatus {
        health,
        quiescent,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Sonde A (issue #282 investigation): healthy crate, not yet quiescent.
    #[test]
    fn parses_ok_not_quiescent() {
        let msg = json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
            "params":{"health":"ok","quiescent":false,"message":null}});
        let status = parse_server_status(&msg).expect("parses");
        assert_eq!(status.health, ServerHealthLevel::Ok);
        assert!(!status.quiescent);
        assert_eq!(status.message, None);
    }

    /// Sonde B: the exact shape a nested-workspace target sends.
    #[test]
    fn parses_error_with_message() {
        let msg = json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
            "params":{"health":"error","quiescent":true,"message":"Failed to load workspaces."}});
        let status = parse_server_status(&msg).expect("parses");
        assert_eq!(status.health, ServerHealthLevel::Error);
        assert!(status.quiescent);
        assert_eq!(
            status.message.as_deref(),
            Some("Failed to load workspaces.")
        );
    }

    /// Sonde D: a `warning` health can also end with `quiescent: true`.
    #[test]
    fn parses_warning() {
        let msg = json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
            "params":{"health":"warning","quiescent":true,
                      "message":"Failed to discover workspace"}});
        let status = parse_server_status(&msg).expect("parses");
        assert_eq!(status.health, ServerHealthLevel::Warning);
    }

    #[test]
    fn null_message_parses_as_none() {
        let msg = json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
            "params":{"health":"ok","quiescent":true,"message":null}});
        let status = parse_server_status(&msg).expect("parses");
        assert_eq!(status.message, None);
    }

    /// An unrecognized `health` string must not fail the parse — forward
    /// compatibility with a rust-analyzer value this module doesn't know.
    #[test]
    fn unknown_health_string_maps_to_unknown_not_a_parse_failure() {
        let msg = json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
            "params":{"health":"degraded","quiescent":false,"message":null}});
        let status = parse_server_status(&msg).expect("parses despite unrecognized health");
        assert_eq!(status.health, ServerHealthLevel::Unknown);
    }

    #[test]
    fn non_server_status_message_is_none() {
        let msg = json!({"jsonrpc":"2.0","method":"$/progress",
            "params":{"token":"a","value":{"kind":"begin"}}});
        assert!(parse_server_status(&msg).is_none());
    }

    #[test]
    fn missing_quiescent_field_is_none() {
        // A malformed serverStatus (missing the field `readiness` depends
        // on) must not parse as `Some` with a fabricated default — that
        // would let a debounce state machine reason about a signal that
        // never actually arrived.
        let msg = json!({"jsonrpc":"2.0","method":"experimental/serverStatus",
            "params":{"health":"ok"}});
        assert!(parse_server_status(&msg).is_none());
    }
}
