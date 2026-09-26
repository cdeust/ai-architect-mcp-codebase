// resolver::calls — Stage-3b Phase 2: Call resolution
//
// Extracted from resolver.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// resolution types/helpers exactly as when this lived in one module.

use super::*;
use crate::graph_store::{call_rel_table, call_site_rel_table};

mod crate_scope;
mod gates;
mod variant_guard;
use gates::{rust_local_receiver_gate, same_class_receiver_gate};

// ---------------------------------------------------------------------------
// Phase 2: Call resolution
// source: stages/stage-3b.md §5.2
// ---------------------------------------------------------------------------

pub(super) fn resolve_calls(
    store: &GraphStore,
    idx: &SymbolIndex,
    file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    // source: tasks/plan-issues-282-283-284.md §2.3 (lot 6, issue #283
    // palier 3) — a graph indexed by an older build has no such column; the
    // DEFAULT backfills existing rows to "" (rust_local_receiver_gate reads
    // that as "no hint"), same precedent as `lsp_resolver/sites.rs:113`'s
    // `is_resolved` column.
    store.ensure_node_column("CallSite", "receiver_hint", "STRING DEFAULT ''")?;
    // Issues #348 and #349: same precedent; '' reads as "written at the binding".
    store.ensure_node_column("CallSite", "receiver_hint_via", "STRING DEFAULT ''")?;
    let qr = store.execute_query(
        "MATCH (cs:CallSite) RETURN cs.id, cs.callee_name, cs.language, cs.receiver_hint, \
         cs.receiver_hint_via",
    )?;
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    // §10.4 — CallSite nodes whose callee was resolved to a graph target.
    let mut resolved_ids: Vec<String> = Vec::new();
    // Issue #353: CallSites left unresolved because every candidate is a twin
    // of one item under mutually exclusive `#[cfg]` predicates.
    let mut twin_site_ids: Vec<String> = Vec::new();
    // Sites of a twin set the build profile decided: resolved, so a `cfg_twins`
    // reason an earlier pass wrote on them is stale.
    let mut selected_site_ids: Vec<String> = Vec::new();
    let twins = super::cfg_select::TwinView::load(store);
    // Issue #358: the facts of the latest index pass, not those of the pass
    // that parsed each file.
    let evidence = store.crate_evidence();
    // Issue #370: what each associated function returns, and every variant.
    let assoc = super::receiver::AssocFacts::load(store);

    for row in &qr.rows {
        if row.len() < 5 {
            continue;
        }
        let callee = &row[1];
        // Macro invocations (`name!(...)`) are a distinct reference kind,
        // resolved exclusively by resolver_layers::run_macro_expansion.
        // Counting them here too would attempt (and fail) a plain-function
        // lookup for every macro call, double-counting the same physical
        // CallSite into both phases' total_refs — the root cause of the
        // >1.0 / undercounted-denominator half of issue #28.
        // A Rust macro only: Ruby keeps the `!` of `save!` in its callee name.
        if crate::graph_store::is_rust_macro_site(callee, &row[2]) {
            continue;
        }
        total += 1;
        let mut tally = CallTally {
            resolved: &mut resolved,
            unresolved: &mut unresolved,
            twin_sites: &mut twin_site_ids,
            selected_sites: &mut selected_site_ids,
        };
        let graph = GraphContext {
            idx,
            file_imports,
            twins: &twins,
            evidence: &evidence,
            assoc: &assoc,
        };
        let row_input = RowInput {
            cs_id: &row[0],
            callee,
            language: &row[2],
            receiver_hint: &row[3],
            receiver_hint_via: &row[4],
        };
        if resolve_one_call_site(&graph, buf, &row_input, &mut tally) {
            resolved_ids.push(row[0].clone());
        }
    }
    let id_refs: Vec<&str> = resolved_ids.iter().map(|s| s.as_str()).collect();
    store.mark_nodes_resolved("CallSite", &id_refs)?;
    super::cfg_twins::persist_twin_reason(store, &twin_site_ids, &selected_site_ids)?;
    Ok((resolved, total, unresolved))
}

/// Resolves one CallSite row: gathers the caller-side evidence, delegates
/// ambiguity/confidence entirely to `resolve_single_call` (which in turn
/// delegates to the single `ambiguity_policy` module — issue #30), then
/// stages or records the outcome. Extracted from `resolve_calls` to keep
/// that function an orchestration loop (scan + accumulate) — resolution,
/// edge-kind reclassification, schema validation, and metric counting are
/// each owned by a single downstream helper (`resolve_single_call`,
/// `stage_call_edge`, `check_known_rel_table` inside it, and `CallTally`
/// respectively) instead of being interleaved inline. source: issue #32.
///
/// postcondition: returns `true` iff the callee resolved to a real graph
/// target (mirrors the former inline `resolved > resolved_before` check) —
/// the caller uses this to flip `CallSite.is_resolved` (§10.4). Behavior is
/// identical to the pre-extraction inline loop body.
fn resolve_one_call_site(
    graph: &GraphContext,
    buf: &mut EdgeBuffer,
    row: &RowInput,
    tally: &mut CallTally,
) -> bool {
    let provider = crate::language_provider::provider_for(row.language);
    let caller_qn = extract_caller_from_callsite_id(row.cs_id);
    let file_id = extract_file_prefix_or_self(&caller_qn);
    let caller_label = determine_caller_label(graph.idx, &caller_qn);

    let site = CallSite {
        cs_id: row.cs_id,
        callee: row.callee,
        caller_qn: &caller_qn,
        caller_label: &caller_label,
        receiver_hint: row.receiver_hint,
        receiver_hint_via: row.receiver_hint_via,
    };
    let ctx = ResolveContext {
        idx: graph.idx,
        provider,
        file_imports: graph.file_imports,
        evidence: graph.evidence,
        assoc: graph.assoc,
    };
    let resolved_before = *tally.resolved;
    match resolve_single_call(&ctx, &site, &file_id) {
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
            stage_call_edge(buf, &site, &matched, tally);
        }
        // Genuinely ambiguous (no evidence tier discriminates the
        // candidates): labeled and dropped rather than guessed — see
        // resolve_single_call's doc comment for why this beats a
        // deterministic tiebreak here (issue #30).
        PolicyResolution::Ambiguous { candidates } => {
            // Issue #353: twins of one item under exclusive `#[cfg]` gates are
            // resolved only when the build decides which one it compiles.
            match super::cfg_select::choose(graph.twins, site.caller_qn, &candidates) {
                Some(twin) => {
                    let matched = MatchedCall {
                        target: twin,
                        evidence: ambiguity_policy::Evidence::CfgSelected,
                        confidence: ambiguity_policy::confidence_for(
                            ambiguity_policy::Evidence::CfgSelected,
                        ),
                    };
                    stage_call_edge(buf, &site, &matched, tally);
                    tally.selected_sites.push(site.cs_id.to_string());
                }
                None => record_ambiguous(&site, tally, &candidates, graph.twins),
            }
        }
        PolicyResolution::NotFound => {
            record_call_unresolved(&site, tally, "no target found".to_string())
        }
    }
    // The callee resolved to a graph target — flip the CallSite's
    // is_resolved (§10.4). Applies to both Calls and Uses edges (both mean
    // "target found").
    *tally.resolved > resolved_before
}

/// Records one unresolved `Calls` reference with the given reason.
fn record_call_unresolved(site: &CallSite, tally: &mut CallTally, reason: String) {
    tally.unresolved.push(UnresolvedRef {
        kind: "Calls".to_string(),
        from_id: site.cs_id.to_string(),
        target_text: site.callee.to_string(),
        reason,
    });
}

/// One row from the `CallSite` scan, grouped so downstream helpers take a
/// single reference instead of four loose string parameters.
struct CallSite<'a> {
    cs_id: &'a str,
    callee: &'a str,
    caller_qn: &'a str,
    caller_label: &'a str,
    /// The parser-attached issue #283 palier 3 (lot 6) hint; "" means none.
    receiver_hint: &'a str,
    /// How the parser derived `receiver_hint`; see `RowInput`.
    receiver_hint_via: &'a str,
}

/// Read-only lookup context shared by one `resolve_single_call` invocation
/// — groups the graph index, the language provider, and the file-import
/// map so the function takes one reference for "static" state instead of
/// three loose parameters (CONTRIBUTING.md §4.2, ≤4 parameters).
struct ResolveContext<'a> {
    idx: &'a SymbolIndex,
    provider: &'a dyn crate::language_provider::LanguageProvider,
    file_imports: &'a HashMap<String, Vec<String>>,
    /// The Cargo facts of the latest index pass (issue #358).
    evidence: &'a crate::graph_store::import_roots::CrateEvidence,
    /// Return types of associated functions and enum variants (issue #370).
    assoc: &'a super::receiver::AssocFacts,
}

/// The per-run, read-only graph state `resolve_one_call_site` needs —
/// grouped (CONTRIBUTING.md §4.2, ≤4 parameters) so adding the receiver
/// gate's Rust-language check to this call chain didn't push the function
/// over the parameter cap it was already at before this lot.
struct GraphContext<'a> {
    idx: &'a SymbolIndex,
    file_imports: &'a HashMap<String, Vec<String>>,
    /// `cfg_active` of every `#[cfg]` twin (issue #353).
    twins: &'a super::cfg_select::TwinView,
    evidence: &'a crate::graph_store::import_roots::CrateEvidence,
    assoc: &'a super::receiver::AssocFacts,
}

/// One `CallSite` scan row, grouped for the same reason as `GraphContext`.
struct RowInput<'a> {
    cs_id: &'a str,
    callee: &'a str,
    language: &'a str,
    /// The parser-attached issue #283 palier 3 (lot 6) hint; "" means none.
    receiver_hint: &'a str,
    /// `RECEIVER_HINT_VIA_RETURN_TYPE` when the hint was read off a free
    /// function's return type (issues #348 and #349), "" when it was written
    /// at the binding, and for a graph indexed before the column existed.
    receiver_hint_via: &'a str,
}

/// A resolved callee plus the evidence/confidence the policy attached to it.
struct MatchedCall<'a> {
    target: &'a SymbolEntry,
    evidence: ambiguity_policy::Evidence,
    confidence: f64,
}

/// An ambiguous callee: dropped and labeled. When every candidate is a twin of
/// one item under exclusive `#[cfg]` gates (issue #353) the label is `cfg_twins`
/// and the site is queued for the reason to be persisted.
fn record_ambiguous(
    site: &CallSite,
    tally: &mut CallTally,
    candidates: &[SymbolEntry],
    view: &super::cfg_select::TwinView,
) {
    let twins = super::cfg_twins::are_twins_of_one_item(view, candidates);
    if twins {
        tally.twin_sites.push(site.cs_id.to_string());
    }
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
}

/// Running counters for `resolve_calls`, grouped so helpers take one
/// reference instead of two separate mutable accumulator parameters.
struct CallTally<'a> {
    resolved: &'a mut u64,
    unresolved: &'a mut Vec<UnresolvedRef>,
    /// Ids of the sites whose every candidate is a cfg twin (issue #353).
    twin_sites: &'a mut Vec<String>,
    /// Ids of the sites a twin set was resolved for by the build profile.
    selected_sites: &'a mut Vec<String>,
}

/// Stages the Calls/Uses edge for one resolved callee, or records why it
/// couldn't be staged (no rel table for the label combination).
///
/// The label-pair rule itself lives in `graph_store::call_rel_table`, shared
/// with the LSP fallback pass so the two resolvers cannot drift.
fn stage_call_edge(
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

/// Resolves one callee reference via the shared ambiguity policy (issue
/// #30), through `call_evidence::resolve_two_pass` (issue #29). Both the
/// qualified/import-matched path and the unqualified path build the
/// evidence available at the call site and delegate — `ambiguity_policy`
/// remains the ONLY place that decides ambiguity/confidence; this function
/// (and call_evidence.rs) only gather and represent evidence for it.
///
/// Deliberately never applies a deterministic tiebreak: a genuinely
/// ambiguous reference (no evidence tier discriminates the candidates) is
/// left unresolved (`Ambiguous`) rather than guessed. This is stricter than
/// the issue's suggested "deterministic tiebreak to preserve recall" —
/// tested against `tests/graph_accuracy.rs` (the repo's ground-truth
/// gate), the deterministic tiebreak measurably regressed precision: e.g.
/// `infrastructure/pg_store.py` has a bare `_now_iso()` call inside a
/// method, name-ambiguous between the module-level function and an
/// unrelated same-named method; Python's own scoping resolves it to the
/// function, and at the time neither candidate carried evidence our tiers
/// modelled, so tiebreaking picked the wrong (method) target and introduced
/// 2 false Calls edges (Calls F1 dropped 1.0 -> 0.5). Dropping the edge
/// (labeled `ambiguous (N candidates)`) kept precision but did lose that
/// real edge; since #335 `visible_candidates` applies Python's scoping (a
/// bare call never names a method), so the method is no longer a candidate
/// and the call resolves to the function without any tiebreak.
/// The tiebreaking variant (`resolve_deterministic`) was removed from
/// ambiguity_policy as dead code (PR #38); recover it from git history if
/// a future caller prefers recall over precision for its own ambiguity
/// class.
///
/// precondition: `site.callee` is the raw callee spelling as parsed (for
/// Kotlin, per issue #29, this preserves a package/object qualifier — see
/// parser/kotlin/extract/g2.rs::qualifier_or_tail — but never a
/// value-receiver, which the parser strips back to a bare name before it
/// reaches here); `file_id` is the caller's file path; `site.caller_qn`/
/// `site.caller_label` identify the calling symbol (used only by the
/// same-class receiver gate below); `site.receiver_hint` is the
/// parser-attached issue #283 palier 3 hint (used only by the local-receiver
/// gate below).
/// postcondition: the returned `Resolution` depends only on the candidate
/// set and the evidence context — never directly on whether the callee was
/// spelled qualified or unqualified — EXCEPT for the two receiver gates,
/// each of which is itself evidence (the callee's own receiver
/// spelling, or the parser's derived local-binding type), not a
/// spelling-dependent shortcut around the policy.
fn resolve_single_call(
    ctx: &ResolveContext,
    site: &CallSite,
    file_id: &str,
) -> PolicyResolution<SymbolEntry> {
    let callee = site.callee;
    if let Some(res) = same_class_receiver_gate(ctx, site) {
        return res;
    }
    if let Some(res) = rust_local_receiver_gate(ctx, site, file_id) {
        return res;
    }

    // Fully qualified: the callee's own spelling is the import-match
    // evidence. Unqualified: evidence is the file's own import list.
    let (last, imports_hint): (&str, Vec<String>) =
        if callee.contains("::") || callee.contains(ctx.provider.import_separator()) {
            (
                ctx.provider.import_last_segment(callee),
                vec![callee.to_string()],
            )
        } else {
            (
                callee,
                ctx.file_imports.get(file_id).cloned().unwrap_or_default(),
            )
        };
    let Some(candidates) = ctx.idx.by_name.get(last) else {
        return PolicyResolution::NotFound;
    };
    let candidates = visible_candidates(ctx, site, candidates, last != callee);
    let candidates = variant_guard::drop_struct_targets(ctx, callee, candidates);
    let ev = crate::call_evidence::CallEvidence {
        imports_hint: &imports_hint,
        caller_file: file_id,
    };
    crate::call_evidence::resolve_two_pass(
        &candidates,
        |e: &SymbolEntry| e.qualified_name.clone(),
        |e: &SymbolEntry| extract_file_prefix_or_self(&e.qualified_name),
        ctx.provider,
        &ev,
    )
}

/// The candidates a call at `site` can actually name under the caller
/// language's scoping rules, before any evidence is weighed.
///
/// Rust block-scoped fn items (issue #327): a nested fn shadows every other
/// candidate inside its enclosing callable and is invisible outside. Python
/// (`bare_call_binds_methods == false`): an unqualified call never names a
/// method, so a same-named method is not a rival of the module function it
/// really calls (pg_store.py `_now_iso()`, which the #30 policy used to drop
/// as ambiguous, #335) and is never a target on its own.
fn visible_candidates<'a>(
    ctx: &ResolveContext,
    site: &CallSite,
    candidates: &'a [SymbolEntry],
    qualified: bool,
) -> std::borrow::Cow<'a, [SymbolEntry]> {
    use std::borrow::Cow;
    if ctx.provider.language() == "rust" {
        return Cow::Owned(nested_scope::visible_candidates(
            ctx.idx,
            candidates,
            site.caller_qn,
            qualified,
        ));
    }
    if qualified || ctx.provider.bare_call_binds_methods() {
        return Cow::Borrowed(candidates);
    }
    Cow::Owned(
        candidates
            .iter()
            .filter(|c| c.label != "Method")
            .cloned()
            .collect(),
    )
}
