use crate::epistemic::{self, Boundary};
use crate::graph_store::{community_ids, cypher_str, process_names, GraphStore, SymbolMatch};

use super::impact_reasons;

/// A reverse-dependency edge endpoint, carried as a re-queryable handle
/// (id + qualified_name + label) rather than a flattened name string, so a
/// consumer can keep traversing the graph through MCP from this node instead
/// of receiving a terminal summary.
/// source: anti-flattening principle — `get_impact` must hand back traversal
/// handles, not a dead-end digest (the caller continues via get_symbol /
/// get_context / query_graph on `id`).
pub struct ImpactNode {
    pub id: String,
    pub qualified_name: String,
    pub label: String,
    /// Confidence of the reverse-dependency edge connecting this dependent to
    /// the target: the stored `confidence` edge property when present, else the
    /// per-relation-type floor (`epistemic::relation_confidence_floor`). A value
    /// < 1.0 means this dependency was resolved heuristically and may be wrong.
    pub confidence: f64,
}

pub struct ImpactResult {
    pub communities: Vec<String>,
    pub processes: Vec<String>,
    /// Reverse `Calls` — functions/methods that call the target.
    pub callers: Vec<ImpactNode>,
    /// Reverse `Imports` — files/modules that import the target.
    pub importers: Vec<ImpactNode>,
    /// Reverse `Uses` — symbols that use the target type.
    pub users: Vec<ImpactNode>,
    /// Reverse `Implements` — types that implement the target trait.
    pub implementors: Vec<ImpactNode>,
    /// Reverse `References` — non-code files (Markdown docs, shell scripts)
    /// that point at the target file (issue #205). File→file, no symbol
    /// resolution; kept as its own section (not folded into `importers`,
    /// which is code-dependency-only) so a caller can see the code/reference
    /// split explicitly.
    pub references: Vec<ImpactNode>,
    /// Whether the captured dependent set is exhaustive (`Exact`) or a lower
    /// bound on true impact (`LowerBound`) — see `epistemic` module.
    pub epistemic: Boundary,
    /// Human-readable carriers of epistemic uncertainty (empty when `Exact`).
    pub epistemic_reasons: Vec<String>,
    /// Count of unresolved `CallSite` nodes (`is_resolved = false`) whose
    /// `callee_name` names this target by its bare identifier but were never
    /// linked to a `Calls` edge. Structured twin of the prose carrier
    /// `unresolved_callsite_reason` folds into `epistemic_reasons`: an empty
    /// `callers` list with this field > 0 means "N call sites name the
    /// target and none resolved", which is a materially different finding
    /// from "this symbol has no callers" (`callers` empty AND this field
    /// == 0) — the two were indistinguishable to a caller before this field
    /// existed. source: issue #283 (a).
    pub unresolved_callsites_naming_target: u64,
    /// Of `unresolved_callsites_naming_target`, how many are attributed
    /// "outside the compiled Cargo targets" — a file the LSP pass proved sits
    /// outside every target `cargo metadata` reports, so the language server
    /// never had a chance to resolve them (`lsp_resolver::pass`,
    /// `indexer::cargo_targets::TargetMap`). 0 on a graph indexed before this
    /// lot (no `CallSite.unresolved_reason` column) — see
    /// `impact_reasons::unresolved_callsite_attribution`. source: issue #284
    /// (lot 5, the `get_impact` half — the LSP-pass half is `lsp_resolve`).
    pub unresolved_callsites_outside_targets: u64,
}

// ---------------------------------------------------------------------------
// get_impact — blast radius for a symbol
// source: stages/stage-3c.md §5 get_impact
// ---------------------------------------------------------------------------

pub fn get_impact(store: &GraphStore, qualified_name: &str) -> Result<ImpactResult, String> {
    let esc = cypher_str(qualified_name);

    let communities = collect_communities(store, qualified_name);
    let processes = collect_processes(store, qualified_name);

    // Reverse-dependency traversal — the actual blast radius. The tool is
    // named for impact analysis but previously returned only community +
    // process membership; the set of symbols that DEPEND ON the target
    // (callers, importers, users, implementors) is what a "what breaks if I
    // change this?" query needs. Each is a re-queryable handle so the caller
    // can keep walking the graph through MCP rather than stopping at a digest.
    let callers = reverse_dependents(store, &esc, "Calls_");
    let importers = reverse_dependents(store, &esc, "Imports_");
    let users = reverse_dependents(store, &esc, "Uses_");
    let implementors = reverse_dependents(store, &esc, "Implements_");
    // Doc/script cross-references (issue #205) — kept separate from
    // `importers` since these are file-level References_File_File edges, not
    // a code dependency.
    let references = reverse_dependents(store, &esc, "References_");

    let target_bare_name = crate::bridge::last_segment(qualified_name);
    let deps = ReverseDependents {
        callers: &callers,
        importers: &importers,
        users: &users,
        implementors: &implementors,
        references: &references,
    };
    let (attribution, epistemic_reasons, epistemic) =
        resolve_epistemic(store, &esc, target_bare_name, &deps);

    Ok(ImpactResult {
        communities,
        processes,
        callers,
        importers,
        users,
        implementors,
        references,
        epistemic,
        epistemic_reasons,
        unresolved_callsites_naming_target: attribution.total,
        unresolved_callsites_outside_targets: attribution.outside_targets,
    })
}

/// Communities (`MemberOf_<Label>_Community`) the target symbol belongs to,
/// across every `SYMBOL_LABELS` kind. Takes the RAW target — `membership`
/// escapes it.
fn collect_communities(store: &GraphStore, target: &str) -> Vec<String> {
    let symbol = SymbolMatch::IdOrQualifiedName(target);
    super::SYMBOL_LABELS
        .iter()
        .flat_map(|label| community_ids(store, label, symbol))
        .collect()
}

/// Processes (`ParticipatesIn_<Label>_Process`) the target symbol
/// participates in. Takes the RAW target — `membership` escapes it.
fn collect_processes(store: &GraphStore, target: &str) -> Vec<String> {
    let symbol = SymbolMatch::IdOrQualifiedName(target);
    ["Function", "Method"]
        .iter()
        .flat_map(|label| process_names(store, label, symbol))
        .collect()
}

/// Resolves the epistemic boundary of a `get_impact` result: the unresolved
/// call-site attribution (issue #283 (a)/#284 (lot 5), see
/// `impact_reasons::unresolved_callsite_attribution`), the prose reasons
/// built from it (plus dynamic-dispatch / heuristic-edge / file-fan-in
/// carriers), and the resulting `Boundary`. `esc` must already be a
/// `cypher_str`-quoted literal; `target_bare_name` is the target's own
/// unescaped unqualified identifier (see `get_impact`).
///
/// The attribution is computed once here and threaded into both the prose
/// reason and the structured counts returned to the caller — the query used
/// to run twice before this field existed; a single result is now the source
/// every surface reads, so they can never disagree on N.
fn resolve_epistemic(
    store: &GraphStore,
    esc: &str,
    target_bare_name: &str,
    deps: &ReverseDependents,
) -> (impact_reasons::UnresolvedCallsiteAttribution, Vec<String>, Boundary) {
    let attribution = impact_reasons::unresolved_callsite_attribution(store, target_bare_name);
    let epistemic_reasons = impact_reasons::build_epistemic_reasons(store, esc, &attribution, deps);
    let epistemic = if epistemic_reasons.is_empty() {
        Boundary::Exact
    } else {
        Boundary::LowerBound
    };
    (attribution, epistemic_reasons, epistemic)
}

/// The five reverse-dependency slices `get_impact` collects, grouped into one
/// handle so downstream epistemic-reason helpers take a parameter object
/// instead of five positional slices (coding-standards §4.4: >4 parameters is
/// a missing data type). `pub(super)` — `impact_reasons` (a sibling module
/// under `clustering`) reads it too.
pub(super) struct ReverseDependents<'a> {
    pub(super) callers: &'a [ImpactNode],
    pub(super) importers: &'a [ImpactNode],
    pub(super) users: &'a [ImpactNode],
    pub(super) implementors: &'a [ImpactNode],
    pub(super) references: &'a [ImpactNode],
}

/// Reverse-traverses every `REL_TABLES` edge whose name starts with `prefix`,
/// binding the escaped target to the edge's `to` endpoint and returning the
/// `from` endpoints as re-queryable handles. This is the inverse of the
/// forward "what does X reference?" walk: "what references X?".
///
/// `esc` must already be a `cypher_str`-quoted literal (see `get_impact`).
/// CallSite sources are skipped: they carry no `qualified_name`, so they
/// would contribute null-name noise — the function-level caller is the
/// meaningful dependent and is captured by the direct `Calls_Function_*` /
/// `Calls_Method_*` edges the resolver also emits.
fn reverse_dependents(store: &GraphStore, esc: &str, prefix: &str) -> Vec<ImpactNode> {
    // Per-relation-type floor for this traversal, used when an edge carries no
    // stored confidence (older graphs / untyped edges) so a dependent is never
    // silently treated as fully confident. source: epistemic module.
    let floor = epistemic::relation_confidence_floor(prefix);
    let mut out = Vec::new();
    for &(rel, from_label, to_label) in crate::graph_store::REL_TABLES {
        if !rel.starts_with(prefix) {
            continue;
        }
        if from_label == crate::graph_store::NODE_CALL_SITE {
            continue;
        }
        // Bind the edge as `r` so its stored `confidence` property surfaces; a
        // resolution/provenance edge carries it, structural edges do not (then
        // `r.confidence` is empty and we fall back to the relation floor).
        //
        // Gate every `qualified_name` reference on whether the label declares
        // that column. lbug raises a hard Binder exception (not NULL) when a
        // query binds a property the matched label lacks, which silently drops
        // the ENTIRE query's rows. Before this gate, any Imports_/Uses_/… table
        // whose `to`-label (e.g. File) or `from`-label lacked `qualified_name`
        // errored out, so File-targeted and File-sourced dependents never
        // surfaced — the exact gap that made IaC manifest→File edges (issue #63)
        // invisible to get_impact, and that already dropped plain
        // Imports_File_File light-links. source: graph_store::label_has_qualified_name.
        let b_pred = if crate::graph_store::label_has_qualified_name(to_label) {
            format!("b.id = {esc} OR b.qualified_name = {esc}")
        } else {
            format!("b.id = {esc}")
        };
        let a_qn = if crate::graph_store::label_has_qualified_name(from_label) {
            "a.qualified_name"
        } else {
            // No qualified_name column — the id is the traversal handle.
            "a.id"
        };
        let cypher = format!(
            "MATCH (a:{from_label})-[r:{rel}]->(b:{to_label}) \
             WHERE {b_pred} \
             RETURN a.id, {a_qn}, r.confidence"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            out.extend(
                qr.rows
                    .iter()
                    .filter(|row| row.len() >= 2)
                    .map(|row| dependent_node(row, from_label, floor)),
            );
        }
    }
    out
}

/// Builds one `ImpactNode` from a `reverse_dependents` result row
/// (`a.id, a.qualified_name/a.id, r.confidence`, already length-checked by
/// the caller). Falls back to `floor` when the row has no parseable
/// confidence (structural edges carry no `r.confidence` property).
fn dependent_node(row: &[String], from_label: &str, floor: f64) -> ImpactNode {
    let confidence = row
        .get(2)
        .and_then(|c| c.parse::<f64>().ok())
        .unwrap_or(floor);
    ImpactNode {
        id: row[0].clone(),
        qualified_name: row[1].clone(),
        label: from_label.to_string(),
        confidence,
    }
}

#[cfg(test)]
#[path = "impact_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "impact_outside_targets_tests.rs"]
mod outside_targets_tests;
