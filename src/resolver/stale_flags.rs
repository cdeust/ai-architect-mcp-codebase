// resolver::stale_flags: which call sites the static resolver leaves open and
// may safely mark `is_resolved = false` (issue #353, second review round).
//
// An incremental refresh purges a target with its file and leaves the flag of an
// unchanged caller's site as it was, `true`. The next resolve then finds no
// target, or an ambiguous one, and used to leave the flag: the graph claimed a
// resolution it no longer had. The reset covers every site the resolver reports
// unresolved, EXCEPT a site that holds a per-site `Calls_CallSite_*` edge: an
// edge is the evidence of another tier (the language server), and the static
// resolver failing to find a target must not undo it.

use super::UnresolvedRef;
use crate::graph_store::{cypher_str, GraphStore, REL_TABLES};
use std::collections::HashSet;

/// Flags every open site `is_resolved = false` and writes the `cfg_twins` reason
/// on those of `twin_site_ids` that are open (no edge from another tier).
pub(super) fn settle_open_sites(
    store: &GraphStore,
    unresolved: &[UnresolvedRef],
    flagged: &HashSet<&str>,
    twin_site_ids: Vec<String>,
) -> Result<(), String> {
    let open = open_sites(store, unresolved, flagged, &twin_site_ids)?;
    let ids: Vec<&str> = open.iter().map(String::as_str).collect();
    store.mark_callsites_unresolved(&ids)?;
    let twins: Vec<String> = twin_site_ids
        .into_iter()
        .filter(|id| open.contains(id))
        .collect();
    super::cfg_twins::persist_twin_reason(store, &twins)
}

/// Sites to reset: the `Calls` references left unresolved this run that were
/// flagged resolved before it (or are `extra`, the twin sites whose reason is
/// about to be written), without any outgoing per-site call edge.
pub(super) fn open_sites(
    store: &GraphStore,
    unresolved: &[UnresolvedRef],
    flagged: &HashSet<&str>,
    extra: &[String],
) -> Result<HashSet<String>, String> {
    let mut candidates: HashSet<String> = unresolved
        .iter()
        .filter(|u| u.kind == "Calls" && flagged.contains(u.from_id.as_str()))
        .map(|u| u.from_id.clone())
        .collect();
    candidates.extend(extra.iter().cloned());
    if candidates.is_empty() {
        return Ok(candidates);
    }
    let ids: Vec<String> = candidates.iter().cloned().collect();
    for chunk in ids.chunks(500) {
        let list = chunk
            .iter()
            .map(|id| cypher_str(id))
            .collect::<Vec<_>>()
            .join(", ");
        for &(table, from, _) in REL_TABLES {
            if from != "CallSite" || !table.starts_with("Calls_") {
                continue;
            }
            let rows = store.execute_query(&format!(
                "MATCH (c:CallSite)-[r:{table}]->() WHERE c.id IN [{list}] RETURN c.id"
            ))?;
            for row in rows.rows {
                candidates.remove(&row[0]);
            }
        }
    }
    Ok(candidates)
}
