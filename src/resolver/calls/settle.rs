// resolver::calls::settle — what becomes of one call site once its callee is
// looked up: the edge staged for a resolved one, the reason recorded for an open
// one. Moved out of `calls.rs` to keep that file under the §4.1 cap.

use super::reason::{Failure, Gated};
use super::*;
use crate::graph_store::{call_rel_table, call_site_rel_table};

/// What the outcome of one call site is settled against: the graph state, the
/// lookup context, the site and its file.
pub(super) struct Located<'a> {
    pub(super) graph: &'a GraphContext<'a>,
    pub(super) ctx: &'a ResolveContext<'a>,
    pub(super) site: &'a CallSite<'a>,
    pub(super) file_id: &'a str,
}

/// Stages the edge of a resolved outcome, or records why the site stays open.
/// Returns the label of a target found when no edge could be staged for it.
pub(super) fn settle(
    buf: &mut EdgeBuffer,
    tally: &mut CallTally,
    at: &Located,
    (resolution, decline): Gated,
) -> Option<String> {
    match resolution {
        PolicyResolution::Resolved {
            target,
            evidence,
            confidence,
        } => {
            let matched = MatchedCall {
                target: &target,
                evidence,
                confidence,
            };
            staged_label(buf, tally, at.site, &matched)
        }
        // Genuinely ambiguous (no evidence tier discriminates the
        // candidates): labeled and dropped rather than guessed — see
        // resolve_single_call's doc comment for why this beats a
        // deterministic tiebreak here (issue #30).
        PolicyResolution::Ambiguous { candidates } => settle_ambiguous(buf, tally, at, &candidates),
        PolicyResolution::NotFound => {
            record_call_unresolved(at.site, tally, "no target found".to_string());
            record_reason(
                at.ctx,
                at.site,
                at.file_id,
                &Failure::NotFound(decline),
                tally,
            );
            None
        }
    }
}

/// Stages the edge of `matched`; the label of its target when none was staged.
pub(super) fn staged_label(
    buf: &mut EdgeBuffer,
    tally: &mut CallTally,
    site: &CallSite,
    matched: &MatchedCall,
) -> Option<String> {
    let resolved_before = *tally.resolved;
    stage_call_edge(buf, site, matched, tally);
    (*tally.resolved == resolved_before).then(|| matched.target.label.clone())
}

pub(super) fn settle_ambiguous(
    buf: &mut EdgeBuffer,
    tally: &mut CallTally,
    at: &Located,
    candidates: &[SymbolEntry],
) -> Option<String> {
    // Issue #353: twins of one item under exclusive `#[cfg]` gates are
    // resolved only when the build decides which one it compiles.
    if let Some(twin) = super::cfg_select::choose(at.graph.twins, at.site.caller_qn, candidates) {
        let matched = MatchedCall {
            target: twin,
            evidence: ambiguity_policy::Evidence::CfgSelected,
            confidence: ambiguity_policy::confidence_for(ambiguity_policy::Evidence::CfgSelected),
        };
        return staged_label(buf, tally, at.site, &matched);
    }
    let twins = record_ambiguous(at.site, tally, candidates, at.graph.twins);
    let failure = Failure::Ambiguous {
        count: candidates.len(),
        twins,
    };
    record_reason(at.ctx, at.site, at.file_id, &failure, tally);
    None
}

/// Queues the reason `failure` gives the site (issue #393).
pub(super) fn record_reason(
    ctx: &ResolveContext,
    site: &CallSite,
    file_id: &str,
    failure: &Failure,
    tally: &mut CallTally,
) {
    let (why, detail) = reason::classify(ctx, site, file_id, failure);
    tally.reasons.push((site.cs_id.to_string(), why, detail));
}

/// Records one unresolved `Calls` reference with the given reason.
pub(super) fn record_call_unresolved(site: &CallSite, tally: &mut CallTally, reason: String) {
    tally.unresolved.push(UnresolvedRef {
        kind: "Calls".to_string(),
        from_id: site.cs_id.to_string(),
        target_text: site.callee.to_string(),
        reason,
    });
}

/// A resolved callee plus the evidence/confidence the policy attached to it.
pub(super) struct MatchedCall<'a> {
    target: &'a SymbolEntry,
    evidence: ambiguity_policy::Evidence,
    confidence: f64,
}

/// An ambiguous callee: dropped and labeled. When every candidate is a twin of
/// one item under exclusive `#[cfg]` gates (issue #353) the label is `cfg_twins`;
/// returns whether it is.
pub(super) fn record_ambiguous(
    site: &CallSite,
    tally: &mut CallTally,
    candidates: &[SymbolEntry],
    view: &super::cfg_select::TwinView,
) -> bool {
    let twins = super::cfg_twins::are_twins_of_one_item(view, candidates);
    let label = if twins {
        crate::graph_store::CALLSITE_UNRESOLVED_REASON_CFG_TWINS
    } else {
        "ambiguous"
    };
    record_call_unresolved(
        site,
        tally,
        format!("{label} ({} candidates)", candidates.len()),
    );
    twins
}

/// Running counters for `resolve_calls`, grouped so helpers take one
/// reference instead of two separate mutable accumulator parameters.
pub(super) struct CallTally<'a> {
    pub(super) resolved: &'a mut u64,
    pub(super) unresolved: &'a mut Vec<UnresolvedRef>,
    /// Why each site left open is open (issue #393).
    pub(super) reasons: &'a mut Vec<crate::graph_store::callsite_reasons::SiteReasonRow>,
}

/// Stages the Calls/Uses edge for one resolved callee, or records why it
/// couldn't be staged (no rel table for the label combination).
///
/// The label-pair rule itself lives in `graph_store::call_rel_table`, shared
/// with the LSP fallback pass so the two resolvers cannot drift.
pub(super) fn stage_call_edge(
    buf: &mut EdgeBuffer,
    site: &CallSite,
    matched: &MatchedCall,
    tally: &mut CallTally,
) {
    let target = matched.target;
    let Some(rel) = call_rel_table(site.caller_label, &target.label) else {
        return record_call_unresolved(
            site,
            tally,
            format!(
                "no rel table for {} -> {} (callsite-as-call)",
                site.caller_label, target.label
            ),
        );
    };
    // Schema guard: every name `call_rel_table` returns is in REL_TABLES
    // today, so this defends against a future schema edit rather than
    // filtering live traffic. A drop is already logged inside.
    if !check_known_rel_table(&rel, site.caller_qn, &target.id) {
        return;
    }
    // All three `AddOutcome` variants mean the reference resolved to a real
    // target (see `AddOutcome` doc comment); they differ only in whether a
    // DB write is queued.
    let method = ambiguity_policy::resolution_label(matched.evidence);
    buf.add(&rel, site.caller_qn, &target.id, matched.confidence, method);
    // The per-site row (issue #335) records the same resolution at call-site
    // granularity. It is not a second reference: `tally` stays untouched so
    // `total_edges` keeps counting resolved references, not rows.
    if let Some(site_rel) = call_site_rel_table(&target.label) {
        buf.add(site_rel, site.cs_id, &target.id, matched.confidence, method);
    }
    *tally.resolved += 1;
}
