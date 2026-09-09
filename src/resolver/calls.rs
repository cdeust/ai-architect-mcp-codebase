// resolver::calls — Stage-3b Phase 2: Call resolution
//
// Extracted from resolver.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// resolution types/helpers exactly as when this lived in one module.

use super::*;
use crate::graph_store::call_rel_table;

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
    let qr = store.execute_query(
        "MATCH (cs:CallSite) RETURN cs.id, cs.callee_name, cs.language, cs.receiver_hint",
    )?;
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    // §10.4 — CallSite nodes whose callee was resolved to a graph target.
    let mut resolved_ids: Vec<String> = Vec::new();

    for row in &qr.rows {
        if row.len() < 4 {
            continue;
        }
        let callee = &row[1];
        // Macro invocations (`name!(...)`) are a distinct reference kind,
        // resolved exclusively by resolver_layers::run_macro_expansion.
        // Counting them here too would attempt (and fail) a plain-function
        // lookup for every macro call, double-counting the same physical
        // CallSite into both phases' total_refs — the root cause of the
        // >1.0 / undercounted-denominator half of issue #28.
        if callee.ends_with('!') {
            continue;
        }
        total += 1;
        let mut tally = CallTally {
            resolved: &mut resolved,
            unresolved: &mut unresolved,
        };
        let graph = GraphContext { idx, file_imports };
        let row_input = RowInput {
            cs_id: &row[0],
            callee,
            language: &row[2],
            receiver_hint: &row[3],
        };
        if resolve_one_call_site(&graph, buf, &row_input, &mut tally) {
            resolved_ids.push(row[0].clone());
        }
    }
    let id_refs: Vec<&str> = resolved_ids.iter().map(|s| s.as_str()).collect();
    store.mark_nodes_resolved("CallSite", &id_refs)?;
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
    };
    let ctx = ResolveContext {
        idx: graph.idx,
        provider,
        file_imports: graph.file_imports,
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
        PolicyResolution::Ambiguous { candidates } => record_call_unresolved(
            &site,
            tally,
            format!("ambiguous ({} candidates)", candidates.len()),
        ),
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
}

/// Read-only lookup context shared by one `resolve_single_call` invocation
/// — groups the graph index, the language provider, and the file-import
/// map so the function takes one reference for "static" state instead of
/// three loose parameters (CONTRIBUTING.md §4.2, ≤4 parameters).
struct ResolveContext<'a> {
    idx: &'a SymbolIndex,
    provider: &'a dyn crate::language_provider::LanguageProvider,
    file_imports: &'a HashMap<String, Vec<String>>,
}

/// The per-run, read-only graph state `resolve_one_call_site` needs —
/// grouped (CONTRIBUTING.md §4.2, ≤4 parameters) so adding the receiver
/// gate's Rust-language check to this call chain didn't push the function
/// over the parameter cap it was already at before this lot.
struct GraphContext<'a> {
    idx: &'a SymbolIndex,
    file_imports: &'a HashMap<String, Vec<String>>,
}

/// One `CallSite` scan row, grouped for the same reason as `GraphContext`.
struct RowInput<'a> {
    cs_id: &'a str,
    callee: &'a str,
    language: &'a str,
    /// The parser-attached issue #283 palier 3 (lot 6) hint; "" means none.
    receiver_hint: &'a str,
}

/// A resolved callee plus the evidence/confidence the policy attached to it.
struct MatchedCall<'a> {
    target: &'a SymbolEntry,
    evidence: ambiguity_policy::Evidence,
    confidence: f64,
}

/// Running counters for `resolve_calls`, grouped so helpers take one
/// reference instead of two separate mutable accumulator parameters.
struct CallTally<'a> {
    resolved: &'a mut u64,
    unresolved: &'a mut Vec<UnresolvedRef>,
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
    buf.add(
        &rel,
        site.caller_qn,
        &target.id,
        matched.confidence,
        ambiguity_policy::resolution_label(matched.evidence),
    );
    *tally.resolved += 1;
}

/// Rust `self.<m>` / `Self::<m>` on a Method caller: resolved against the
/// enclosing impl's type BEFORE `resolve_single_call`'s by-name lookup,
/// which would otherwise try (and always fail) to find a symbol literally
/// named "self.<m>" — see receiver.rs's module doc and issue #283.
///
/// precondition: `site.callee`/`site.caller_qn`/`site.caller_label` come
/// from the same `CallSite` row `resolve_single_call` was called with.
/// postcondition: `None` when the gate does not apply (non-Rust caller, not
/// a Method, or the callee isn't `self`/`Self`-shaped) — the caller must
/// fall through to the pre-existing by-name path. `Some(_)` is a final
/// answer for a self/Self-shaped callee and is NEVER a bare-name-lookup
/// fallback: `resolve_receiver_bound` returns `Some(NotFound)` /
/// `Some(Ambiguous)` rather than `None` for those outcomes (receiver.rs
/// postcondition) — this is what preserves the zero-false-callers property
/// this module's tests defend.
fn rust_receiver_gate(
    ctx: &ResolveContext,
    site: &CallSite,
) -> Option<PolicyResolution<SymbolEntry>> {
    if ctx.provider.language() != "rust" || site.caller_label != "Method" {
        return None;
    }
    let form = receiver::classify(site.callee);
    if !matches!(
        form,
        receiver::ReceiverForm::SelfValue(_) | receiver::ReceiverForm::SelfType(_)
    ) {
        return None;
    }
    receiver::resolve_receiver_bound(ctx.idx, &form, receiver::impl_qn_of(site.caller_qn))
}

/// Rust `<local>.<m>` on ANY caller (not gated to `Method`, unlike the
/// `self`/`Self` gate above — a free function's local variable qualifies
/// exactly as well as a method's): resolved against the parser-attached
/// `CallSite.receiver_hint` BEFORE `resolve_single_call`'s by-name lookup,
/// which would otherwise try (and always fail) to find a symbol literally
/// named "<local>.<m>" — see receiver.rs's module doc and issue #283 palier
/// 3 (lot 6).
///
/// precondition: `site.callee`/`site.receiver_hint` come from the same
/// `CallSite` row `resolve_single_call` was called with; `file_id` is the
/// caller's own file id.
/// postcondition: `None` when the gate does not apply (non-Rust caller,
/// callee isn't `<ident>.<m>`-shaped, or `receiver_hint` is empty — no hint
/// attached, meaning the parser found no once-bound-and-typed local) — the
/// caller must fall through to the pre-existing by-name path. `Some(_)` is a
/// final answer for a hinted local receiver and is NEVER a bare-name-lookup
/// fallback, mirroring `rust_receiver_gate`'s zero-false-callers discipline.
fn rust_local_receiver_gate(
    ctx: &ResolveContext,
    site: &CallSite,
    file_id: &str,
) -> Option<PolicyResolution<SymbolEntry>> {
    if ctx.provider.language() != "rust" || site.receiver_hint.is_empty() {
        return None;
    }
    let form = receiver::classify(site.callee);
    let receiver::ReceiverForm::Local { m, .. } = form else {
        return None;
    };
    Some(receiver::resolve_local_receiver_bound(
        ctx.idx,
        site.receiver_hint,
        &m,
        file_id,
    ))
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
/// function, but neither candidate carries evidence our tiers model, so
/// tiebreaking picked the wrong (method) target and introduced 2 false
/// Calls edges (Calls F1 dropped 1.0 -> 0.5). Dropping the edge (labeled
/// `ambiguous (N candidates)`) matches the pre-issue-#30 unqualified
/// behavior exactly, so no real edges are lost — only the mislabeling and
/// the qualified-path's arbitrary-`candidates[0]` guess are fixed.
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
/// `site.caller_label` identify the calling symbol (used only by the Rust
/// `self`/`Self` receiver gate below); `site.receiver_hint` is the
/// parser-attached issue #283 palier 3 hint (used only by the local-receiver
/// gate below).
/// postcondition: the returned `Resolution` depends only on the candidate
/// set and the evidence context — never directly on whether the callee was
/// spelled qualified or unqualified — EXCEPT for the two Rust receiver
/// gates, each of which is itself evidence (the callee's own receiver
/// spelling, or the parser's derived local-binding type), not a
/// spelling-dependent shortcut around the policy.
fn resolve_single_call(
    ctx: &ResolveContext,
    site: &CallSite,
    file_id: &str,
) -> PolicyResolution<SymbolEntry> {
    let callee = site.callee;
    if let Some(res) = rust_receiver_gate(ctx, site) {
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
    let candidates = match ctx.idx.by_name.get(last) {
        Some(c) => c,
        None => return PolicyResolution::NotFound,
    };
    let ev = crate::call_evidence::CallEvidence {
        imports_hint: &imports_hint,
        caller_file: file_id,
    };
    crate::call_evidence::resolve_two_pass(
        candidates,
        |e: &SymbolEntry| e.qualified_name.clone(),
        |e: &SymbolEntry| extract_file_prefix_or_self(&e.qualified_name),
        ctx.provider,
        &ev,
    )
}
