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
                "macro_sites_count": r.macro_sites_count,
                "unlinked_file_check": super::lsp_coverage::unlinked_check_json(&r.unlinked_check),
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
/// `completed_unresolved`; a pass with nothing to resolve stays `completed`,
/// which is not a failure.
/// Issue #315: "nothing to resolve" means the pass entered with zero sites
/// (`resolved + failed + skipped + outside_targets == 0`, the `pass.rs`
/// counter identity). A pass that entered with sites and attributed every
/// one outside the compiled Cargo targets (#284, lot 5 — no request was
/// ever issued for them) still resolved nothing, so it is
/// `completed_unresolved` too; the cause stays readable from
/// `outside_targets_count`. Any resolution keeps `completed`: an
/// outside-targets site next to resolved ones (a Kani harness beside a
/// working crate) is not by itself a failure.
pub(super) fn completed_state(result: &LspResolutionResult) -> &'static str {
    let unresolved_sites =
        result.failed_count + result.skipped_count + result.outside_targets_count;
    if result.resolved_count == 0 && unresolved_sites > 0 {
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
            macro_sites_count: 0,
            unlinked_check: crate::lsp_client::UnlinkedFileCheck::default(),
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

    /// Issue #284 (lot 5) intent kept under #315: outside-targets sites next
    /// to resolved ones do not flip the state.
    #[test]
    fn outside_targets_beside_a_resolution_stays_completed() {
        let mut r = result(2, 0, 0);
        r.outside_targets_count = 5;
        assert_eq!(completed_state(&r), "completed");
    }

    /// Issue #315 (probe E): a `[workspace] members = []` root analysed with
    /// its only crate not listed as a member — every one of the 461 sites
    /// the pass entered with was attributed outside the compiled targets
    /// and none resolved. That is "ran and resolved nothing", not "nothing
    /// to resolve".
    #[test]
    fn every_site_outside_targets_with_none_resolved_is_completed_unresolved() {
        let mut r = result(0, 0, 0);
        r.outside_targets_count = 461;
        assert_eq!(completed_state(&r), "completed_unresolved");
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
