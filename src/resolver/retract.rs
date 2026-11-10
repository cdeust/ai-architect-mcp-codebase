// resolver::retract: the static call rows a pass no longer decides (issue #443,
// first resolver slice).
//
// `resolve_calls` decides every call site again on every pass, but the
// `EdgeBuffer` only adds rows. A row written by an earlier pass for a site that
// now resolves elsewhere, or not at all, stayed in the graph, and so did the
// site's `is_resolved = true`. Example (Codex review of #478, P1): `a.rs` calls
// `beta()`, unique in the first pass; an incremental pass adds a second `beta`,
// the next resolve finds the call ambiguous, yet the `unique-match` rows of the
// first pass remained. A full index of the same tree has neither the rows nor
// the flag.
//
// So the static tiers' `Calls_*` rows that existed before the pass and that no
// phase of the pass staged or met again are deleted after the flush, and the
// call sites that hold no per-site call row afterwards are set back to
// `is_resolved = false` (the pass already wrote their open reason).
//
// Rows of every other writer are left alone: the language server
// (`lsp-definition`), the macro layer (`macro-expansion*`, reset by
// `reset_macro_expansion` before the pass), and the structural rows. Only rows
// whose `resolution_method` is a static tier label are owned here; a row the
// pass meets again keeps its properties (trace ingest's `observed_count`).
// Known gap, accepted for this slice: `Uses_*`, `Imports_*`, `Implements_*` and
// `Extends_*` rows are still only added.

use crate::ambiguity_policy::{resolution_label, Evidence};
use crate::graph_store::{cypher_str, GraphStore, NODE_CALL_SITE, REL_TABLES};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// A resolution row as the `EdgeBuffer` keys it: (rel table, from id, to id).
pub(super) type RowKey = (String, String, String);

/// The `resolution_method` labels the static call tiers write. The macro
/// tiers are not listed: their own reset deletes their rows before each pass.
fn static_call_methods() -> Vec<&'static str> {
    [
        Evidence::UniqueGlobal,
        Evidence::ReceiverBound,
        Evidence::ImportMatch,
        Evidence::ReceiverLocalBinding,
        Evidence::ReceiverReturnType,
        Evidence::SameFileUnique,
        Evidence::PackageProximity,
        Evidence::CfgSelected,
    ]
    .into_iter()
    .map(resolution_label)
    .collect()
}

fn method_list() -> String {
    static_call_methods()
        .into_iter()
        .map(cypher_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn call_tables() -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str)> {
    REL_TABLES
        .iter()
        .filter(|(rel, _, _)| rel.starts_with("Calls_"))
}

/// The static-tier `Calls_*` rows in the graph before the pass runs.
pub(super) fn load_static_call_rows(store: &GraphStore) -> Result<HashSet<RowKey>, String> {
    let methods = method_list();
    let mut rows = HashSet::new();
    for &(rel, from, to) in call_tables() {
        let cypher = format!(
            "MATCH (a:{from})-[r:{rel}]->(b:{to}) WHERE r.resolution_method IN [{methods}] \
             RETURN a.id, b.id"
        );
        // A table an older graph lacks has no row to retract.
        let Ok(qr) = store.execute_query(&cypher) else {
            continue;
        };
        for row in qr.rows {
            if row.len() >= 2 {
                rows.insert((rel.to_string(), row[0].clone(), row[1].clone()));
            }
        }
    }
    Ok(rows)
}

/// Deletes the rows of `owned` absent from `met` (the persisted rows the pass
/// decided again), then reopens the call sites left without a per-site call
/// row. Returns the number of rows deleted.
///
/// Precondition: `owned` was read by `load_static_call_rows` before the pass,
/// and the pass's buffer has been flushed. Idempotent: a second call with the
/// same arguments finds nothing left to delete and reopens the same sites.
pub(super) fn retract_unmet(
    store: &GraphStore,
    owned: &HashSet<RowKey>,
    met: &HashSet<RowKey>,
) -> Result<u64, String> {
    let stale: BTreeSet<&RowKey> = owned.difference(met).collect();
    if stale.is_empty() {
        return Ok(0);
    }
    let labels: BTreeMap<&str, (&str, &str)> = call_tables()
        .map(|&(rel, from, to)| (rel, (from, to)))
        .collect();
    let methods = method_list();
    let mut sites: BTreeSet<&str> = BTreeSet::new();
    for (rel, from_id, to_id) in &stale {
        let Some(&(from, to)) = labels.get(rel.as_str()) else {
            continue;
        };
        store.execute_query(&format!(
            "MATCH (a:{from})-[r:{rel}]->(b:{to}) WHERE a.id = {} AND b.id = {} \
             AND r.resolution_method IN [{methods}] DELETE r",
            cypher_str(from_id),
            cypher_str(to_id),
        ))?;
        if from == NODE_CALL_SITE {
            sites.insert(from_id.as_str());
        }
    }
    reopen_sites_without_row(store, &sites)?;
    Ok(stale.len() as u64)
}

/// Sets `is_resolved = false` on the sites of `sites` that hold no per-site
/// call row of any writer. A site another tier still resolves keeps its flag.
fn reopen_sites_without_row(store: &GraphStore, sites: &BTreeSet<&str>) -> Result<(), String> {
    if sites.is_empty() {
        return Ok(());
    }
    let ids: Vec<&str> = sites.iter().copied().collect();
    let mut with_row: HashSet<String> = HashSet::new();
    for chunk in ids.chunks(500) {
        let list = chunk
            .iter()
            .map(|id| cypher_str(id))
            .collect::<Vec<_>>()
            .join(", ");
        for &(rel, from, _) in call_tables() {
            if from != NODE_CALL_SITE {
                continue;
            }
            let qr = store.execute_query(&format!(
                "MATCH (c:CallSite)-[r:{rel}]->() WHERE c.id IN [{list}] RETURN c.id"
            ))?;
            with_row.extend(qr.rows.into_iter().filter_map(|r| r.into_iter().next()));
        }
    }
    let open: Vec<&str> = ids
        .into_iter()
        .filter(|id| !with_row.contains(*id))
        .collect();
    store.mark_callsites_unresolved(&open)
}
