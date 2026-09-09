// clustering::impact_reasons — epistemic-boundary reason building, plus the
// unresolved-`CallSite` attribution query `get_impact` reads them from.
//
// Split from `impact.rs` (issue #284, lot 5) when this lot's outside-target
// attribution would have pushed that file past the §4.1 500-line cap. A pure
// move for `dynamic_dispatch_reason`/`heuristic_edge_reason`/
// `is_file_node`/`missing_reference_indexing_reason`/`lookup_target_label`/
// `build_epistemic_reasons`; `unresolved_callsite_count_naming` is replaced
// by `unresolved_callsite_attribution` (same total, plus the new
// outside-target split), and `unresolved_callsite_reason`'s wording gained
// the outside-target clause.

use crate::epistemic;
use crate::graph_store::{cypher_str, GraphStore, NODE_CALL_SITE};
use crate::language_provider::extract_file_prefix_or_self;

use super::impact::ReverseDependents;
use super::ImpactNode;

/// Everything `get_impact` needs about the unresolved `CallSite` nodes that
/// name a target by its bare identifier: the total (issue #283 (a)) and, when
/// the graph's `CallSite.unresolved_reason` column exists (issue #284, lot
/// 5), how many of them — and which files — are attributed "outside the
/// compiled Cargo targets".
pub(super) struct UnresolvedCallsiteAttribution {
    pub(super) total: u64,
    pub(super) outside_targets: u64,
    pub(super) outside_target_files: Vec<String>,
}

/// Counts unresolved `CallSite` nodes (`is_resolved = false`) whose
/// `callee_name` names `target_bare_name` — the same evidence `resolve_calls`
/// (src/resolver/calls.rs) would have consumed had resolution succeeded,
/// generic across every language and every reason resolution can fail.
/// `target_bare_name` is the target's own unescaped unqualified identifier
/// (never a full qualified_name).
///
/// A `callee_name` names the target under exactly the three call shapes the
/// parsers emit: a bare call (`response_of`), a receiver call
/// (`s.response_of`, `self.response_of`), or a call qualified by a type/path
/// not yet resolved (`Type::response_of`). All three end in the bare
/// identifier, so one exact-or-suffix comparison covers every shape without
/// re-deriving the parser's own callee-spelling grammar.
///
/// Issue #284 (lot 5): when `CallSite.unresolved_reason` exists on this
/// graph, also partitions the total into how many of those sites — and which
/// files — are attributed "outside the compiled Cargo targets" (written by
/// `lsp_resolver::pass::LspPass::mark_resolved`). Read-only: the presence
/// check (`node_column_exists`) never `ALTER`s — `get_impact` runs against
/// `graph_cache::open_cached`'s shared, `readOnlyHint`-advertised handle — so
/// a graph indexed before this lot degrades to the plain count instead of the
/// hard Binder exception a query naming a missing property would otherwise
/// raise (measured 2026-08-24, lbug 0.19.1).
pub(super) fn unresolved_callsite_attribution(
    store: &GraphStore,
    target_bare_name: &str,
) -> UnresolvedCallsiteAttribution {
    let esc_bare = cypher_str(target_bare_name);
    let esc_dot_suffix = cypher_str(&format!(".{target_bare_name}"));
    let esc_scope_suffix = cypher_str(&format!("::{target_bare_name}"));
    let name_filter = format!(
        "cs.is_resolved = false AND (cs.callee_name = {esc_bare} OR cs.callee_name ENDS WITH \
         {esc_dot_suffix} OR cs.callee_name ENDS WITH {esc_scope_suffix})"
    );

    let has_reason_col = store
        .node_column_exists(NODE_CALL_SITE, "unresolved_reason")
        .unwrap_or(false);
    if !has_reason_col {
        return UnresolvedCallsiteAttribution {
            total: count_unresolved(store, &name_filter),
            outside_targets: 0,
            outside_target_files: Vec::new(),
        };
    }

    let cypher = format!(
        "MATCH (cs:{NODE_CALL_SITE}) WHERE {name_filter} RETURN cs.id, cs.unresolved_reason"
    );
    let rows = store
        .execute_query(&cypher)
        .map(|qr| qr.rows)
        .unwrap_or_default();
    let total = rows.len() as u64;
    let mut outside_targets = 0u64;
    let mut outside_target_files = std::collections::BTreeSet::new();
    for row in &rows {
        let is_outside = row
            .get(1)
            .map(|r| r == crate::graph_store::CALLSITE_UNRESOLVED_REASON_OUTSIDE_TARGETS)
            .unwrap_or(false);
        if !is_outside {
            continue;
        }
        outside_targets += 1;
        if let Some(id) = row.first() {
            outside_target_files.insert(extract_file_prefix_or_self(id));
        }
    }
    UnresolvedCallsiteAttribution {
        total,
        outside_targets,
        outside_target_files: outside_target_files.into_iter().collect(),
    }
}

/// Plain `COUNT(cs)` over `name_filter` — the whole answer on a graph that
/// predates `unresolved_reason`, where no attribution is possible.
///
/// Postcondition: return value is never negative — `COUNT(cs)` in Cypher is
/// bounded below by 0 by construction, so the u64 return type is exact, not a
/// truncating cast of a signed count.
fn count_unresolved(store: &GraphStore, name_filter: &str) -> u64 {
    let cypher = format!("MATCH (cs:{NODE_CALL_SITE}) WHERE {name_filter} RETURN count(cs)");
    store
        .execute_query(&cypher)
        .ok()
        .and_then(|qr| {
            qr.rows
                .first()
                .and_then(|r| r.first())
                .and_then(|c| c.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

/// Assembles the epistemic-boundary reasons for a `get_impact` result:
/// dynamic-dispatch surface, heuristically-resolved edges, unresolved call
/// sites naming the target, and (issue #205) unindexed markdown/shell
/// references. An empty result means `Boundary::Exact`. `esc` must already
/// be a `cypher_str`-quoted literal; `target_bare_name` is the target's own
/// (unescaped) unqualified identifier — the last `::`/`.`-segment of
/// `qualified_name`, e.g. `crate::bridge::last_segment`.
///
/// Epistemic boundary: the dependent set is a LOWER BOUND on true impact when
/// (a) the target is a dynamic-dispatch surface (calls through the
/// interface/trait bind to an implementor at runtime and are not
/// exhaustively static), (b) any contributing edge was resolved
/// heuristically (confidence < 1.0), (c) the graph holds unresolved
/// `CallSite` nodes that name the target by its bare identifier but were
/// never resolved into a `Calls`/`Uses` edge (resolution failed — static or
/// LSP — for any reason: dynamic dispatch, a resolver gap, a timeout, an
/// unsupported language tier, or a file outside the compiled Cargo targets —
/// issue #284), or (d) the target is a File with zero inbound references in a
/// graph that predates reference-edge indexing. Otherwise it is exact.
/// source: epistemic module contract.
///
/// `attribution` is the count/split `get_impact` already computed (via
/// `unresolved_callsite_attribution`) for the structured `ImpactResult`
/// fields — passed in rather than requeried, so the prose reason and the
/// structured counts can never drift apart.
pub(super) fn build_epistemic_reasons(
    store: &GraphStore,
    esc: &str,
    attribution: &UnresolvedCallsiteAttribution,
    deps: &ReverseDependents,
) -> Vec<String> {
    let mut reasons = Vec::new();

    if let Some(reason) = dynamic_dispatch_reason(store, esc, deps.implementors) {
        reasons.push(reason);
    }
    if let Some(reason) = heuristic_edge_reason(deps) {
        reasons.push(reason);
    }
    if let Some(reason) = unresolved_callsite_reason(attribution) {
        reasons.push(reason);
    }
    // File-level fan-in honesty (issue #205): when the target is itself a
    // File and this graph contains markdown/shell files but zero
    // References_File_File edges anywhere, code-only fan-in over it is a
    // lower bound — either the graph predates reference-edge indexing, or
    // (much less likely on a doc/script-heavy repo) these files are
    // genuinely unreferenced. This graph cannot tell those two cases apart,
    // so it is reported rather than silently assumed complete.
    if deps.references.is_empty() && is_file_node(store, esc) {
        if let Some(reason) = missing_reference_indexing_reason(store) {
            reasons.push(reason);
        }
    }

    reasons
}

/// `Some(reason)` when the target resolves to a dynamic-dispatch surface
/// (trait/interface) — see `build_epistemic_reasons` doc for the argument.
fn dynamic_dispatch_reason(
    store: &GraphStore,
    esc: &str,
    implementors: &[ImpactNode],
) -> Option<String> {
    let label = lookup_target_label(store, esc)?;
    if !epistemic::is_dynamic_dispatch_surface(&label) {
        return None;
    }
    Some(format!(
        "target is a {label} (dynamic-dispatch surface): call sites that \
         invoke it polymorphically are not exhaustively captured by static \
         resolution; the {} implementor(s) and direct callers shown are a \
         lower bound",
        implementors.len()
    ))
}

/// `Some(reason)` when any reverse-dependency edge was resolved below full
/// confidence — see `build_epistemic_reasons` doc for the argument.
fn heuristic_edge_reason(deps: &ReverseDependents) -> Option<String> {
    let heuristic_count = deps
        .callers
        .iter()
        .chain(deps.importers.iter())
        .chain(deps.users.iter())
        .chain(deps.implementors.iter())
        .chain(deps.references.iter())
        .filter(|n| epistemic::is_heuristic_edge(n.confidence))
        .count();
    if heuristic_count == 0 {
        return None;
    }
    Some(format!(
        "{heuristic_count} reverse-dependency edge(s) were resolved \
         heuristically (confidence < 1.0) and may be incomplete or incorrect"
    ))
}

/// `Some(reason)` when the graph holds unresolved `CallSite` nodes naming the
/// target — see `build_epistemic_reasons` doc for the argument. Takes the
/// already-computed attribution (see that function's doc); does not query.
///
/// Issue #284 (lot 5): when any of the unresolved sites are attributed
/// "outside the compiled Cargo targets", names the count and the file(s) —
/// the distinction between "this resolver has a gap" and "this file was
/// never in the language server's crate graph to begin with, so no resolver
/// gap explains it".
fn unresolved_callsite_reason(attribution: &UnresolvedCallsiteAttribution) -> Option<String> {
    if attribution.total == 0 {
        return None;
    }
    let mut reason = format!(
        "{n} unresolved call site(s) name this symbol — {n} `CallSite` node(s) in the graph \
         reference it by name but were never resolved to a Calls edge; the reported callers \
         are a lower bound",
        n = attribution.total
    );
    if attribution.outside_targets > 0 {
        reason.push_str(&format!(
            "; {out} of them sit in files outside the compiled Cargo targets ({files}) — \
             unresolvable by the language server; static receiver binding may still resolve \
             them",
            out = attribution.outside_targets,
            files = attribution.outside_target_files.join(", ")
        ));
    }
    Some(reason)
}

/// True when `esc` (already `cypher_str`-escaped) identifies an existing File
/// node by id. Distinguishes "target is a File" (where the reference-fan-in
/// honesty check below applies) from "target is a symbol or does not exist"
/// (where it does not).
fn is_file_node(store: &GraphStore, esc: &str) -> bool {
    let cypher = format!("MATCH (f:File) WHERE f.id = {esc} RETURN f.id LIMIT 1");
    matches!(store.execute_query(&cypher), Ok(qr) if qr.rows.iter().any(|r| !r.is_empty()))
}

/// See the call site in `build_epistemic_reasons` for the honesty argument.
/// Returns `None` when there are no markdown/shell files in the graph at all
/// (nothing to be blind to) or when at least one `References_File_File` edge
/// exists anywhere (this graph WAS built with reference-edge indexing).
fn missing_reference_indexing_reason(store: &GraphStore) -> Option<String> {
    let doc_script_count = store
        .execute_query(
            "MATCH (f:File) WHERE f.id ENDS WITH '.md' OR f.id ENDS WITH '.markdown' \
             OR f.id ENDS WITH '.mdx' OR f.id ENDS WITH '.sh' OR f.id ENDS WITH '.bash' \
             RETURN count(f)",
        )
        .ok()
        .and_then(|qr| {
            qr.rows
                .first()
                .and_then(|r| r.first())
                .and_then(|c| c.parse::<i64>().ok())
        })
        .unwrap_or(0);
    if doc_script_count == 0 {
        return None;
    }
    let has_reference_edges = store
        .execute_query("MATCH (:File)-[r:References_File_File]->(:File) RETURN r LIMIT 1")
        .map(|qr| !qr.rows.is_empty())
        .unwrap_or(false);
    if has_reference_edges {
        return None;
    }
    Some(format!(
        "{doc_script_count} markdown/shell file(s) exist in this graph but it \
         contains zero References_File_File edges — either this graph predates \
         reference-edge indexing (issue #205) or these files are genuinely \
         unreferenced; code-only fan-in over a File target is a lower bound"
    ))
}

/// Looks up the symbol label of the impact target by qualified_name or id.
/// Returns the first matching `SYMBOL_LABELS` label, or `None` when the target
/// is not a resolvable symbol node (e.g. a File). `esc` must already be a
/// `cypher_str`-quoted literal (see `get_impact`).
fn lookup_target_label(store: &GraphStore, esc: &str) -> Option<String> {
    for label in super::SYMBOL_LABELS {
        let cypher = format!(
            "MATCH (n:{label}) \
             WHERE n.id = {esc} OR n.qualified_name = {esc} \
             RETURN n.id LIMIT 1"
        );
        let found = store
            .execute_query(&cypher)
            .map(|qr| qr.rows.iter().any(|r| !r.is_empty()))
            .unwrap_or(false);
        if found {
            return Some((*label).to_string());
        }
    }
    None
}
