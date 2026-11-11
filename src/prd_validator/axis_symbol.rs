// prd_validator::axis_symbol — Axis 1: symbol-hallucination findings. Turns
// the `verdict` module's per-claim classification into `ValidationFinding`s.
//
// Split out of prd_validator::mod (coding-standards §4.1/§4.2 — see
// verdict.rs's header for the full split rationale).

use super::verdict::ClaimVerdict;
use super::{ResolvedClaim, ValidationFinding};
use serde_json::{json, Value};

pub(super) fn emit_symbol_hallucination(
    resolved: &[ResolvedClaim],
    verdicts: &[ClaimVerdict],
    findings: &mut Vec<ValidationFinding>,
) {
    for (r, verdict) in resolved.iter().zip(verdicts.iter()) {
        let (severity, message, extra) = match verdict {
            ClaimVerdict::Hallucinated => hallucinated(r),
            ClaimVerdict::Unverifiable(reason) => unverifiable(r, reason),
            ClaimVerdict::Absent => absent(r),
            ClaimVerdict::Resolved | ClaimVerdict::Unscored => continue,
        };
        let mut details = json!({
            "change_kind": r.claim.change_kind,
            "did_you_mean": r.did_you_mean,
        });
        if let (Some(obj), Value::Object(extra)) = (details.as_object_mut(), extra) {
            obj.extend(extra);
        }
        findings.push(ValidationFinding {
            axis: "symbol_hallucination".into(),
            severity: severity.into(),
            message,
            symbol: Some(r.claim.token.clone()),
            details,
        });
    }
}

/// (severity, message, details beyond change_kind + did_you_mean).
type FindingParts = (&'static str, String, Value);

fn hallucinated(r: &ResolvedClaim) -> FindingParts {
    let message = format!(
        "claimed symbol '{}' (change_kind={}) not found in graph",
        r.claim.token, r.claim.change_kind
    );
    ("critical", message, json!({}))
}

fn unverifiable(r: &ResolvedClaim, reason: &str) -> FindingParts {
    let message = format!(
        "claimed symbol '{}' (change_kind={}) could not be verified: {}",
        r.claim.token, r.claim.change_kind, reason
    );
    ("info", message, json!({ "unverifiable_reason": reason }))
}

fn absent(r: &ResolvedClaim) -> FindingParts {
    let message = format!(
        "backticked symbol '{}' matches no code symbol in the graph, by \
         qualified name or by name: a hallucination unless the PRD adds it or \
         it is external (no stage-5 contract, so its change_kind is unknown)",
        r.claim.token
    );
    let extra = json!({ "extraction_mode": "regex_fallback", "matches_by_name": 0 });
    ("warning", message, extra)
}

pub(super) fn emit_unresolved_info(
    resolved: &[ResolvedClaim],
    verdicts: &[ClaimVerdict],
    is_regex_fallback: bool,
    findings: &mut Vec<ValidationFinding>,
) {
    if !is_regex_fallback {
        return;
    }
    for (r, verdict) in resolved.iter().zip(verdicts.iter()) {
        // An `Absent` token already has its warning from
        // `emit_symbol_hallucination`; a second, info-level finding for the
        // same token would contradict it.
        if r.resolved_qn.is_some() || matches!(verdict, ClaimVerdict::Absent) {
            continue;
        }
        findings.push(ValidationFinding {
            axis: "symbol_hallucination".into(),
            severity: "info".into(),
            message: format!(
                "unresolved token '{}' (regex fallback — likely prose)",
                r.claim.token
            ),
            symbol: Some(r.claim.token.clone()),
            details: json!({ "did_you_mean": r.did_you_mean, "extraction_mode": "regex_fallback" }),
        });
    }
}
