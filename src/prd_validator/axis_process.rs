// prd_validator::axis_process — Axis 3: process-impact-contradiction
// findings. Checks whether a resolved claim participates in a process the
// PRD explicitly claimed to be excluded from.
//
// Split out of prd_validator::mod (coding-standards §4.1/§4.2 — see
// verdict.rs's header for the full split rationale).

use super::{ResolvedClaim, ScopeClaim, ValidationFinding};
use crate::graph_store::{process_names, GraphStore, SymbolMatch};
use serde_json::json;

pub(super) fn processes_for_resolved(
    store: &GraphStore,
    resolved: &[ResolvedClaim],
) -> Vec<(String, Vec<String>)> {
    resolved
        .iter()
        .filter_map(|r| {
            r.resolved_qn
                .as_ref()
                .map(|qn| (qn.clone(), processes_of(store, qn)))
        })
        .collect()
}

/// Processes `qualified_name` participates in, through the shared membership
/// traversal, de-duplicated in first-seen order across the two callable labels.
fn processes_of(store: &GraphStore, qualified_name: &str) -> Vec<String> {
    let symbol = SymbolMatch::QualifiedName(qualified_name);
    let mut out: Vec<String> = Vec::new();
    for label in ["Function", "Method"] {
        for name in process_names(store, label, symbol) {
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

pub(super) fn emit_process_impact(
    scope_claims: &[ScopeClaim],
    processes: &[(String, Vec<String>)],
    findings: &mut Vec<ValidationFinding>,
) {
    for claim in scope_claims {
        let excluded = match claim {
            ScopeClaim::ProcessExclusion { processes: ps } => ps,
            _ => continue,
        };
        for (symbol, actual) in processes {
            for hit in actual.iter().filter(|a| excluded.iter().any(|e| e == *a)) {
                findings.push(ValidationFinding {
                    axis: "process_impact".into(),
                    severity: "critical".into(),
                    message: format!(
                        "PRD claims exclusion of process '{}', but '{}' participates in it",
                        hit, symbol
                    ),
                    symbol: Some(symbol.clone()),
                    details: json!({ "process": hit, "excluded_processes": excluded }),
                });
            }
        }
    }
}
