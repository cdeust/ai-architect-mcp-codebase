use crate::epistemic::{self, Boundary};
use crate::graph_store::{cypher_str, GraphStore};

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

    // Find communities this symbol belongs to
    let mut communities = Vec::new();
    for label in super::SYMBOL_LABELS {
        let rel = format!("MemberOf_{label}_Community");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(c:Community) \
             WHERE n.id = {esc} OR n.qualified_name = {esc} \
             RETURN c.id"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if !row.is_empty() {
                    communities.push(row[0].clone());
                }
            }
        }
    }

    // Find processes this symbol participates in
    let mut processes = Vec::new();
    for label in &["Function", "Method"] {
        let rel = format!("ParticipatesIn_{label}_Process");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(p:Process) \
             WHERE n.id = {esc} OR n.qualified_name = {esc} \
             RETURN p.name"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if !row.is_empty() {
                    processes.push(row[0].clone());
                }
            }
        }
    }

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

    // Epistemic boundary: the dependent set above is a LOWER BOUND on true
    // impact when either (a) the target is a dynamic-dispatch surface (calls
    // through the interface/trait bind to an implementor at runtime and are not
    // exhaustively static), or (b) any contributing edge was resolved
    // heuristically (confidence < 1.0). Otherwise it is exact.
    // source: epistemic module contract.
    let target_label = lookup_target_label(store, &esc);
    let mut epistemic_reasons = Vec::new();

    if let Some(label) = &target_label {
        if epistemic::is_dynamic_dispatch_surface(label) {
            epistemic_reasons.push(format!(
                "target is a {label} (dynamic-dispatch surface): call sites that \
                 invoke it polymorphically are not exhaustively captured by static \
                 resolution; the {} implementor(s) and direct callers shown are a \
                 lower bound",
                implementors.len()
            ));
        }
    }

    let heuristic_count = callers
        .iter()
        .chain(importers.iter())
        .chain(users.iter())
        .chain(implementors.iter())
        .filter(|n| epistemic::is_heuristic_edge(n.confidence))
        .count();
    if heuristic_count > 0 {
        epistemic_reasons.push(format!(
            "{heuristic_count} reverse-dependency edge(s) were resolved \
             heuristically (confidence < 1.0) and may be incomplete or incorrect"
        ));
    }

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
        epistemic,
        epistemic_reasons,
    })
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
        if let Ok(qr) = store.execute_query(&cypher) {
            if qr.rows.iter().any(|r| !r.is_empty()) {
                return Some((*label).to_string());
            }
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
            for row in &qr.rows {
                if row.len() >= 2 {
                    let confidence = row
                        .get(2)
                        .and_then(|c| c.parse::<f64>().ok())
                        .unwrap_or(floor);
                    out.push(ImpactNode {
                        id: row[0].clone(),
                        qualified_name: row[1].clone(),
                        label: from_label.to_string(),
                        confidence,
                    });
                }
            }
        }
    }
    out
}
