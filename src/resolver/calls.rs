// resolver::calls — Stage-3b Phase 2: Call resolution
//
// Extracted from resolver.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// resolution types/helpers exactly as when this lived in one module.

use super::*;

mod candidate_scope;
mod crate_scope;
mod declarations;
mod gates;
mod includes;
mod member_calls;
mod reason;
mod settle;
mod variant_guard;
use candidate_scope::qualified_path_gate;
use gates::{rust_local_receiver_gate, same_class_receiver_gate};
use includes::IncludeGraph;
use reason::{Failure, Gated};
use settle::{record_reason, settle, CallTally, Located};

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
    ensure_call_site_columns(store)?;
    let qr = store.execute_query(
        "MATCH (cs:CallSite) RETURN cs.id, cs.callee_name, cs.language, cs.receiver_hint, \
         cs.receiver_hint_via, cs.callee_shape",
    )?;
    let facts = CallFacts::load(store, idx, file_imports)?;
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    // §10.4 — CallSite nodes whose callee was resolved to a graph target.
    let mut resolved_ids: Vec<String> = Vec::new();
    // Issue #393: why each site left open is open.
    let mut reasons: Vec<crate::graph_store::callsite_reasons::SiteReasonRow> = Vec::new();
    for row in &qr.rows {
        let Some(input) = scanned_row(row) else {
            continue;
        };
        total += 1;
        let mut tally = CallTally {
            resolved: &mut resolved,
            unresolved: &mut unresolved,
            reasons: &mut reasons,
        };
        if resolve_one_call_site(&facts.context(), buf, &input, &mut tally) {
            resolved_ids.push(input.cs_id.to_string());
        }
    }
    let id_refs: Vec<&str> = resolved_ids.iter().map(|s| s.as_str()).collect();
    store.mark_nodes_resolved("CallSite", &id_refs)?;
    // One writer for the reasons of this phase (issue #393): a resolved site
    // carries none, an open one its own, and a site a later pass (the language
    // server) resolved keeps none, whatever this pass could not do.
    store.clear_callsite_reasons(&id_refs)?;
    store.write_callsite_reasons(&reasons)?;
    Ok((resolved, total, unresolved))
}

/// A graph indexed by an older build has none of the columns the parser added
/// later; the DEFAULT backfills existing rows.
fn ensure_call_site_columns(store: &GraphStore) -> Result<(), String> {
    // source: tasks/plan-issues-282-283-284.md §2.3 (lot 6, issue #283
    // palier 3) — the DEFAULT backfills existing rows to "" (rust_local_receiver_gate
    // reads that as "no hint"), same precedent as `lsp_resolver/sites.rs:113`'s
    // `is_resolved` column.
    store.ensure_node_column("CallSite", "receiver_hint", "STRING DEFAULT ''")?;
    // Issues #348 and #349: same precedent; '' reads as "written at the binding".
    store.ensure_node_column("CallSite", "receiver_hint_via", "STRING DEFAULT ''")?;
    // Issue #401: same precedent; '' reads as a name.
    store.ensure_node_column("CallSite", "callee_shape", "STRING DEFAULT ''")?;
    Ok(())
}

/// The row of a `CallSite` scan as the resolver reads it; `None` for a short row
/// and for a Rust macro invocation.
fn scanned_row(row: &[String]) -> Option<RowInput<'_>> {
    if row.len() < 6 {
        return None;
    }
    // Macro invocations (`name!(...)`) are a distinct reference kind,
    // resolved exclusively by resolver_layers::run_macro_expansion.
    // Counting them here too would attempt (and fail) a plain-function
    // lookup for every macro call, double-counting the same physical
    // CallSite into both phases' total_refs — the root cause of the
    // >1.0 / undercounted-denominator half of issue #28.
    // A Rust macro only: Ruby keeps the `!` of `save!` in its callee name.
    if crate::graph_store::is_rust_macro_site(&row[1], &row[2]) {
        return None;
    }
    Some(RowInput {
        cs_id: &row[0],
        callee: &row[1],
        language: &row[2],
        receiver_hint: &row[3],
        receiver_hint_via: &row[4],
        callee_shape: &row[5],
    })
}

/// What every call site of a pass is resolved against, read once.
struct CallFacts<'a> {
    idx: &'a SymbolIndex,
    file_imports: &'a HashMap<String, Vec<String>>,
    twins: super::cfg_select::TwinView,
    evidence: crate::graph_store::import_roots::CrateEvidence,
    assoc: super::receiver::AssocFacts,
    imports: super::receiver::ModuleImports,
    callables: crate::graph_store::body_kind::CallableFacts,
    includes: IncludeGraph,
    cpp: member_calls::CppClasses,
}

impl<'a> CallFacts<'a> {
    fn load(
        store: &GraphStore,
        idx: &'a SymbolIndex,
        file_imports: &'a HashMap<String, Vec<String>>,
    ) -> Result<Self, String> {
        // Issue #358: the facts of the latest index pass, not those of the pass
        // that parsed each file.
        let evidence = store.crate_evidence();
        // Issue #370: what each associated function returns, and every variant.
        let assoc = super::receiver::AssocFacts::load(store);
        // Issues #373 and #380: the `use` declarations of every Rust module.
        let imports = super::receiver::ModuleImports::load(store, &evidence);
        let includes = IncludeGraph::load(store)?;
        let cpp = member_calls::CppClasses::load(store, includes.clone());
        Ok(Self {
            idx,
            file_imports,
            twins: super::cfg_select::TwinView::load(store),
            evidence,
            assoc,
            imports,
            // Issue #400: prototypes, macros and `static` functions.
            callables: store.callable_facts(),
            includes,
            cpp,
        })
    }

    fn context(&self) -> GraphContext<'_> {
        GraphContext {
            idx: self.idx,
            file_imports: self.file_imports,
            twins: &self.twins,
            evidence: &self.evidence,
            assoc: &self.assoc,
            imports: &self.imports,
            callables: &self.callables,
            includes: &self.includes,
            cpp: &self.cpp,
        }
    }
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
        callee_shape: row.callee_shape,
    };
    let ctx = ResolveContext {
        idx: graph.idx,
        provider,
        file_imports: graph.file_imports,
        evidence: graph.evidence,
        assoc: graph.assoc,
        imports: graph.imports,
        callables: graph.callables,
        includes: graph.includes,
        cpp: graph.cpp,
    };
    let resolved_before = *tally.resolved;
    let gated = member_calls::open_call(row.language, row.callee_shape, row.receiver_hint_via)
        .unwrap_or_else(|| resolve_single_call(&ctx, &site, &file_id));
    let at = Located {
        graph,
        ctx: &ctx,
        site: &site,
        file_id: &file_id,
    };
    // A target was found but no edge could be staged for it.
    if let Some(label) = settle(buf, tally, &at, gated) {
        record_reason(&ctx, &site, &file_id, &Failure::NoRelTable(&label), tally);
    }
    // The callee resolved to a graph target — flip the CallSite's
    // is_resolved (§10.4). Applies to both Calls and Uses edges (both mean
    // "target found").
    *tally.resolved > resolved_before
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
    /// What the callee is (issue #401); "" for a graph written before the column.
    callee_shape: &'a str,
}

/// Read-only lookup context shared by one `resolve_single_call` invocation
/// — groups the graph index, the language provider, and the file-import
/// map so the function takes one reference for "static" state instead of
/// three loose parameters (coding-standards §4.4, ≤4 parameters).
struct ResolveContext<'a> {
    idx: &'a SymbolIndex,
    provider: &'a dyn crate::language_provider::LanguageProvider,
    file_imports: &'a HashMap<String, Vec<String>>,
    /// The Cargo facts of the latest index pass (issue #358).
    evidence: &'a crate::graph_store::import_roots::CrateEvidence,
    /// Return types of associated functions and enum variants (issue #370).
    assoc: &'a super::receiver::AssocFacts,
    /// The `use` declarations of each Rust module (issues #373, #380).
    imports: &'a super::receiver::ModuleImports,
    /// Prototypes, macros and `static` functions (issue #400).
    callables: &'a crate::graph_store::body_kind::CallableFacts,
    /// The files each C-family file includes (issue #404).
    includes: &'a IncludeGraph,
    /// The C++ classes and their bases (issue #406).
    cpp: &'a member_calls::CppClasses,
}

/// The per-run, read-only graph state `resolve_one_call_site` needs —
/// grouped (coding-standards §4.4, ≤4 parameters) so adding the receiver
/// gate's Rust-language check to this call chain didn't push the function
/// over the parameter cap it was already at before this lot.
struct GraphContext<'a> {
    idx: &'a SymbolIndex,
    file_imports: &'a HashMap<String, Vec<String>>,
    /// `cfg_active` of every `#[cfg]` twin (issue #353).
    twins: &'a super::cfg_select::TwinView,
    evidence: &'a crate::graph_store::import_roots::CrateEvidence,
    assoc: &'a super::receiver::AssocFacts,
    imports: &'a super::receiver::ModuleImports,
    callables: &'a crate::graph_store::body_kind::CallableFacts,
    includes: &'a IncludeGraph,
    cpp: &'a member_calls::CppClasses,
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
    /// What the C or C++ callee is (issue #401); "" for other languages.
    callee_shape: &'a str,
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
fn resolve_single_call(ctx: &ResolveContext, site: &CallSite, file_id: &str) -> Gated {
    let callee = site.callee;
    if let Some(gated) = same_class_receiver_gate(ctx, site) {
        return gated;
    }
    if let Some(gated) = rust_local_receiver_gate(ctx, site, file_id) {
        return gated;
    }
    // A path into std or a foreign crate names no repository item (issue #393).
    if reason::names_external_path(ctx, site, file_id) {
        return (PolicyResolution::NotFound, None);
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
    let Some((candidates, guarded)) = declarations::named_candidates(ctx, site, file_id, last)
    else {
        return (PolicyResolution::NotFound, None);
    };
    if let Some(gated) = qualified_path_gate(ctx, site, &candidates) {
        return gated;
    }
    let ev = crate::call_evidence::CallEvidence {
        imports_hint: &imports_hint,
        caller_file: file_id,
    };
    let resolution = crate::call_evidence::resolve_two_pass(
        &candidates,
        |e: &SymbolEntry| e.qualified_name.clone(),
        |e: &SymbolEntry| extract_file_prefix_or_self(&e.qualified_name),
        ctx.provider,
        &ev,
    );
    (resolution, guarded)
}
