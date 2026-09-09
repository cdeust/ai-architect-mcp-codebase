//! Preserve optional LSP failure evidence while analysis continues.
use crate::lsp_client::{LspResolutionResult, ServerHealth};
use serde_json::{json, Value};

pub(super) enum LspOutcome {
    Disabled,
    Completed(LspResolutionResult),
    Failed(String),
}

impl LspOutcome {
    pub(super) fn status(&self) -> Value {
        match self {
            Self::Disabled => json!({"requested": false, "state": "disabled"}),
            Self::Completed(r) => json!({
                "requested": true,
                "state": completed_state(r),
                "server_health": server_health_json(&r.server_health),
            }),
            Self::Failed(error) => json!({
                "requested": true, "state": "failed", "error": error,
                "fallback": "available_graph",
                "note": "Analysis continued; the graph may include partial LSP results."
            }),
        }
    }

    /// Keep the existing successful counts and disabled null wire shape.
    /// `completed` means the pass returned, not that every site resolved.
    pub(super) fn counts(&self) -> Value {
        match self {
            Self::Completed(r) => json!({
                "resolved_count": r.resolved_count,
                "failed_count": r.failed_count,
                "skipped_count": r.skipped_count,
                "outside_targets_count": r.outside_targets_count,
                "elapsed_ms": r.elapsed_ms,
            }),
            Self::Disabled | Self::Failed(_) => Value::Null,
        }
    }
}

/// Issue #282 §1.2: `completed` used to cover both "ran and resolved
/// something" and "ran and resolved NOTHING" — indistinguishable to a
/// caller that only reads `state`. A pass that answered zero sites while at
/// least one site was actually attempted (`failed` or `skipped` non-zero) is
/// `completed_unresolved`; `resolved == 0` with nothing attempted at all
/// (`failed == 0 && skipped == 0`) stays `completed` — there was nothing to
/// resolve, which is not a failure.
/// Issue #284 (lot 5): `outside_targets` deliberately does NOT feed this
/// check -- a site skipped because its file sits outside every compiled
/// Cargo target was never "attempted" in the sense this state
/// distinguishes (no request was ever issued, so there is no answer to
/// call negative); it surfaces as its own structured count
/// (`outside_targets_count`), not as a variant of failure.
pub(super) fn completed_state(result: &LspResolutionResult) -> &'static str {
    if result.resolved_count == 0 && (result.failed_count > 0 || result.skipped_count > 0) {
        "completed_unresolved"
    } else {
        "completed"
    }
}

/// The wire shape for `lsp_status.server_health` / `lsp_resolve.server_health`
/// (§1.3 MCP contract addition): the server's own reported health, its
/// message (`null` when none), and why the readiness wait that observed it
/// ended.
pub(super) fn server_health_json(health: &ServerHealth) -> Value {
    json!({
        "health": health.level.as_str(),
        "message": health.message,
        "readiness": health.readiness.as_str(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp_client::ReadinessOutcome;

    fn result(resolved: u64, failed: u64, skipped: u64) -> LspResolutionResult {
        LspResolutionResult {
            resolved_count: resolved,
            failed_count: failed,
            skipped_count: skipped,
            outside_targets_count: 0,
            elapsed_ms: 0,
            server_health: ServerHealth {
                level: crate::lsp_client::ServerHealthLevel::Ok,
                message: None,
                readiness: ReadinessOutcome::ServerReportedQuiescent,
            },
        }
    }

    #[test]
    fn zero_resolved_with_failures_is_completed_unresolved() {
        assert_eq!(completed_state(&result(0, 5, 0)), "completed_unresolved");
    }

    #[test]
    fn zero_resolved_with_nothing_attempted_stays_completed() {
        assert_eq!(completed_state(&result(0, 0, 0)), "completed");
    }

    #[test]
    fn any_resolution_stays_completed() {
        assert_eq!(completed_state(&result(3, 2, 0)), "completed");
    }

    #[test]
    fn zero_resolved_with_only_skipped_is_completed_unresolved() {
        // §1.2 rule reads `failed OR skipped`, not `failed` alone — a pass
        // that ran out of budget mid-file (every remaining site `skipped`,
        // none `failed`) is just as much "ran and resolved nothing" as one
        // where every site came back a hard failure.
        assert_eq!(completed_state(&result(0, 0, 4)), "completed_unresolved");
    }

    /// Issue #284 (lot 5): a pass whose every unresolved site sits outside
    /// the compiled Cargo targets attempted nothing in the §1.2 sense (no
    /// `textDocument/definition` request was ever issued) and must stay
    /// `completed`, not flip to `completed_unresolved` — a mutant that folds
    /// `outside_targets_count` into the `failed || skipped` check would fail
    /// this test.
    #[test]
    fn outside_targets_alone_does_not_flip_to_completed_unresolved() {
        let mut r = result(0, 0, 0);
        r.outside_targets_count = 5;
        assert_eq!(completed_state(&r), "completed");
    }

    #[test]
    fn server_health_json_carries_message_and_readiness() {
        let health = ServerHealth {
            level: crate::lsp_client::ServerHealthLevel::Error,
            message: Some("Failed to load workspaces.".to_string()),
            readiness: ReadinessOutcome::ServerReportedQuiescent,
        };
        assert_eq!(
            server_health_json(&health),
            json!({
                "health": "error",
                "message": "Failed to load workspaces.",
                "readiness": "server_reported_quiescent",
            })
        );
    }
}
