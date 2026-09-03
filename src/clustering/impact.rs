use crate::epistemic::{self, Boundary};
use crate::graph_store::{community_ids, cypher_str, process_names, GraphStore, SymbolMatch};

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
    let epistemic_reasons = build_epistemic_reasons(store, &esc, target_bare_name, &deps);
    let epistemic = if epistemic_reasons.is_empty() {
        Boundary::Exact
    } else {
        Boundary::LowerBound
    };

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

/// The five reverse-dependency slices `get_impact` collects, grouped into one
/// handle so downstream epistemic-reason helpers take a parameter object
/// instead of five positional slices (coding-standards §4.4: >4 parameters is
/// a missing data type).
struct ReverseDependents<'a> {
    callers: &'a [ImpactNode],
    importers: &'a [ImpactNode],
    users: &'a [ImpactNode],
    implementors: &'a [ImpactNode],
    references: &'a [ImpactNode],
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
/// unsupported language tier), or (d) the target is a File with zero
/// inbound references in a graph that predates reference-edge indexing.
/// Otherwise it is exact. source: epistemic module contract.
fn build_epistemic_reasons(
    store: &GraphStore,
    esc: &str,
    target_bare_name: &str,
    deps: &ReverseDependents,
) -> Vec<String> {
    let mut reasons = Vec::new();

    if let Some(reason) = dynamic_dispatch_reason(store, esc, deps.implementors) {
        reasons.push(reason);
    }
    if let Some(reason) = heuristic_edge_reason(deps) {
        reasons.push(reason);
    }
    if let Some(reason) = unresolved_callsite_reason(store, target_bare_name) {
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
/// target — see `build_epistemic_reasons` doc for the argument.
fn unresolved_callsite_reason(store: &GraphStore, target_bare_name: &str) -> Option<String> {
    let unresolved_count = unresolved_callsite_count_naming(store, target_bare_name);
    if unresolved_count == 0 {
        return None;
    }
    Some(format!(
        "{unresolved_count} unresolved call site(s) name this symbol — {unresolved_count} \
         `CallSite` node(s) in the graph reference it by name but were never resolved to a \
         Calls edge; the reported callers are a lower bound"
    ))
}

/// Counts unresolved `CallSite` nodes (`is_resolved = false`) whose
/// `callee_name` names `target_bare_name` — the same evidence
/// `resolve_calls` (src/resolver/calls.rs) would have consumed had
/// resolution succeeded, generic across every language and every reason
/// resolution can fail. `target_bare_name` is the target's own unescaped
/// unqualified identifier (never a full qualified_name).
///
/// A `callee_name` names the target under exactly the three call shapes the
/// parsers emit: a bare call (`response_of`), a receiver call
/// (`s.response_of`, `self.response_of`), or a call qualified by a type/path
/// not yet resolved (`Type::response_of`). All three end in the bare
/// identifier, so one exact-or-suffix comparison covers every shape without
/// re-deriving the parser's own callee-spelling grammar.
fn unresolved_callsite_count_naming(store: &GraphStore, target_bare_name: &str) -> i64 {
    let esc_bare = cypher_str(target_bare_name);
    let esc_dot_suffix = cypher_str(&format!(".{target_bare_name}"));
    let esc_scope_suffix = cypher_str(&format!("::{target_bare_name}"));
    let cypher = format!(
        "MATCH (cs:{}) \
         WHERE cs.is_resolved = false AND \
         (cs.callee_name = {esc_bare} OR cs.callee_name ENDS WITH {esc_dot_suffix} \
          OR cs.callee_name ENDS WITH {esc_scope_suffix}) \
         RETURN count(cs)",
        crate::graph_store::NODE_CALL_SITE
    );
    store
        .execute_query(&cypher)
        .ok()
        .and_then(|qr| {
            qr.rows
                .first()
                .and_then(|r| r.first())
                .and_then(|c| c.parse::<i64>().ok())
        })
        .unwrap_or(0)
}

/// True when `esc` (already `cypher_str`-escaped) identifies an existing File
/// node by id. Distinguishes "target is a File" (where the reference-fan-in
/// honesty check below applies) from "target is a symbol or does not exist"
/// (where it does not).
fn is_file_node(store: &GraphStore, esc: &str) -> bool {
    let cypher = format!("MATCH (f:File) WHERE f.id = {esc} RETURN f.id LIMIT 1");
    matches!(store.execute_query(&cypher), Ok(qr) if qr.rows.iter().any(|r| !r.is_empty()))
}

/// See the call site in `get_impact` for the honesty argument. Returns `None`
/// when there are no markdown/shell files in the graph at all (nothing to be
/// blind to) or when at least one `References_File_File` edge exists anywhere
/// (this graph WAS built with reference-edge indexing).
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
