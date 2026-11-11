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
// Sequence, around the pass:
//   1. before it, read the static tiers' `Calls_*` rows (`load_static_call_rows`)
//      and reopen the sites only those rows resolve (`reopen_static_sites`), so
//      the pass writes the open reason of every site it leaves open (the reason
//      writer skips a resolved site; Codex review of #496);
//   2. the pass runs and flushes; it sets back to resolved every site it decides;
//   3. after it, delete the rows of step 1 the pass did not stage or meet again
//      (`retract_unmet`): per-site rows first, then caller-level rows, keeping a
//      caller-level row that a surviving per-site row of the same caller and
//      target still supports (a language-server site; Codex review of #496).
//      No site is reopened here: the sites these rows alone resolved were
//      reopened in step 1 and the pass resolved again the ones it decides (a
//      site that now reaches an Enum, Trait or alias holds no per-site row yet
//      stays resolved; Claude review of #496), and a site another writer also
//      resolves keeps that writer's row.
// A crash between 1 and 3 leaves sites open whose rows still exist: the safe
// side, which the next resolve repairs.
//
// Rows of every other writer are left alone: the language server
// (`lsp-definition`), the macro layer (`macro-expansion*`, reset by
// `reset_macro_expansion` before the pass), and the structural rows. Only rows
// whose `resolution_method` is a static tier label are owned here; a row the
// pass meets again keeps its properties (trace ingest's `observed_count`), and
// the count of a retracted caller-level row moves to `OBSERVED_CALLS_*`.
// `Uses_*`, `Imports_*`, `Implements_*` and `Extends_*` rows are retracted by
// `retract_edges`; `Retraction` runs both around the pass.

use crate::ambiguity_policy::{resolution_label, Evidence};
use crate::graph_store::{cypher_str, GraphStore, NODE_CALL_SITE, REL_TABLES};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// A resolution row as the `EdgeBuffer` keys it: (rel table, from id, to id).
pub(super) type RowKey = (String, String, String);

/// What a pass may retract, read before it runs: the static call rows and the
/// import, use, implements and extends rows of earlier passes.
pub(super) struct Retraction {
    calls: HashSet<RowKey>,
    edges: HashSet<RowKey>,
}

impl Retraction {
    /// Step 1 of both modules: reads the owned rows, reopens the call sites
    /// only those rows resolve, and sets every Import back to unresolved.
    pub(super) fn begin(store: &GraphStore) -> Result<Self, String> {
        let calls = load_static_call_rows(store)?;
        reopen_static_sites(store, &calls)?;
        let edges = super::retract_edges::load_owned_rows(store);
        super::retract_edges::reopen_imports(store)?;
        Ok(Retraction { calls, edges })
    }

    /// The last step: deletes the owned rows absent from `met`, the persisted
    /// rows the pass decided again. Precondition: the pass's buffer is flushed.
    pub(super) fn finish(self, store: &GraphStore, met: &HashSet<RowKey>) -> Result<(), String> {
        retract_unmet(store, &self.calls, met)?;
        super::retract_edges::retract_unmet(store, &self.edges, met)?;
        Ok(())
    }
}

/// The `resolution_method` labels the static call tiers write. The macro
/// tiers are not listed: their own reset deletes their rows before each pass.
pub(super) fn static_call_methods() -> Vec<&'static str> {
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

fn site_tables() -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str)> {
    call_tables().filter(|(_, from, _)| *from == NODE_CALL_SITE)
}

/// The static-tier `Calls_*` rows in the graph before the pass runs.
fn load_static_call_rows(store: &GraphStore) -> Result<HashSet<RowKey>, String> {
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

/// Step 1: sets `is_resolved = false` on the sites of `owned`'s per-site rows
/// that hold no per-site call row of another writer, so the pass that follows
/// writes the reason of each one it leaves open. A site a language-server row
/// also resolves keeps its flag.
fn reopen_static_sites(store: &GraphStore, owned: &HashSet<RowKey>) -> Result<(), String> {
    let sites: BTreeSet<&str> = owned
        .iter()
        .filter(|(rel, _, _)| site_tables().any(|(t, _, _)| t == rel))
        .map(|(_, from, _)| from.as_str())
        .collect();
    reopen_sites_without_row(store, &sites, &method_list())
}

/// Step 3: deletes the rows of `owned` absent from `met` (the persisted rows
/// the pass decided again), per-site rows first, and keeps a caller-level row
/// that a surviving per-site row of the same caller and target supports.
/// Returns the number of rows deleted.
///
/// Precondition: `owned` was read by `load_static_call_rows` before the pass,
/// and the pass's buffer has been flushed. Idempotent: a second call with the
/// same arguments finds nothing left to delete.
fn retract_unmet(
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
    let (per_site, caller_level): (Vec<&RowKey>, Vec<&RowKey>) = stale
        .into_iter()
        .filter(|(rel, _, _)| labels.contains_key(rel.as_str()))
        .partition(|(rel, _, _)| labels[rel.as_str()].0 == NODE_CALL_SITE);
    let mut deleted = 0u64;
    for (rel, from_id, to_id) in &per_site {
        delete_static_row(store, rel, labels[rel.as_str()], from_id, to_id)?;
        deleted += 1;
    }
    for (rel, caller, to_id) in &caller_level {
        let (from, to) = labels[rel.as_str()];
        if supported_by_site(store, caller, to, to_id)? {
            continue;
        }
        if !carry_observed_count(store, rel, (from, to), caller, to_id)? {
            continue;
        }
        delete_static_row(store, rel, (from, to), caller, to_id)?;
        deleted += 1;
    }
    Ok(deleted)
}

/// Moves the runtime `observed_count` that trace ingest put on a caller-level
/// row onto the `OBSERVED_CALLS_*` row ingest writes when no static row exists
/// (Codex review of #496), so retracting the static row keeps the evidence.
/// Returns false, and keeps the row, when there is a count and no
/// `OBSERVED_CALLS_*` table for the label pair to carry it.
fn carry_observed_count(
    store: &GraphStore,
    rel: &str,
    (from, to): (&str, &str),
    caller: &str,
    to_id: &str,
) -> Result<bool, String> {
    let qr = store.execute_query(&format!(
        "MATCH (a:{from})-[r:{rel}]->(b:{to}) WHERE a.id = {} AND b.id = {} \
         AND r.observed_count > 0 RETURN r.observed_count",
        cypher_str(caller),
        cypher_str(to_id),
    ));
    // A graph without the column has no count to carry.
    let Ok(qr) = qr else {
        return Ok(true);
    };
    let count: i64 = qr
        .rows
        .iter()
        .filter_map(|r| r.first().and_then(|c| c.parse::<i64>().ok()))
        .sum();
    if count == 0 {
        return Ok(true);
    }
    let observed = format!("OBSERVED_CALLS_{from}_{to}");
    if !REL_TABLES.iter().any(|(t, _, _)| *t == observed) {
        return Ok(false);
    }
    // The same create-or-accumulate as trace ingest's `upsert_observed_call`
    // (binary crate, `history_handlers`), which this library cannot call.
    let (a, b) = (cypher_str(caller), cypher_str(to_id));
    let existing = store.execute_query(&format!(
        "MATCH (x:{from})-[r:{observed}]->(y:{to}) WHERE x.id = {a} AND y.id = {b} RETURN r.observed_count"
    ))?;
    if existing.rows.is_empty() {
        store.insert_edge(
            &observed,
            caller,
            to_id,
            &[("observed_count", &count.to_string())],
        )?;
    } else {
        store.execute_query(&format!(
            "MATCH (x:{from})-[r:{observed}]->(y:{to}) WHERE x.id = {a} AND y.id = {b} \
             SET r.observed_count = coalesce(r.observed_count, 0) + {count}"
        ))?;
    }
    Ok(true)
}

fn delete_static_row(
    store: &GraphStore,
    rel: &str,
    (from, to): (&str, &str),
    from_id: &str,
    to_id: &str,
) -> Result<(), String> {
    store.execute_query(&format!(
        "MATCH (a:{from})-[r:{rel}]->(b:{to}) WHERE a.id = {} AND b.id = {} \
         AND r.resolution_method IN [{}] DELETE r",
        cypher_str(from_id),
        cypher_str(to_id),
        method_list(),
    ))?;
    Ok(())
}

/// True when a site of `caller` (id `"<caller>::call@…"`, the form
/// `extract_caller_from_callsite_id` reads) still holds a per-site call row,
/// of any writer, to `to_id`.
fn supported_by_site(
    store: &GraphStore,
    caller: &str,
    to: &str,
    to_id: &str,
) -> Result<bool, String> {
    let prefix = cypher_str(&format!("{caller}::call@"));
    for &(rel, _, target) in site_tables() {
        if target != to {
            continue;
        }
        let qr = store.execute_query(&format!(
            "MATCH (c:CallSite)-[r:{rel}]->(b:{to}) WHERE starts_with(c.id, {prefix}) \
             AND b.id = {} RETURN c.id LIMIT 1",
            cypher_str(to_id),
        ))?;
        if !qr.rows.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Sets `is_resolved = false` on the sites of `sites` that hold no per-site
/// call row other than rows whose method is in `ignored` (a quoted list). A
/// site another writer also resolves keeps its flag.
fn reopen_sites_without_row(
    store: &GraphStore,
    sites: &BTreeSet<&str>,
    ignored: &str,
) -> Result<(), String> {
    if sites.is_empty() {
        return Ok(());
    }
    let filter = format!(" AND NOT r.resolution_method IN [{ignored}]");
    let ids: Vec<&str> = sites.iter().copied().collect();
    let mut with_row: HashSet<String> = HashSet::new();
    for chunk in ids.chunks(500) {
        let list = chunk
            .iter()
            .map(|id| cypher_str(id))
            .collect::<Vec<_>>()
            .join(", ");
        for &(rel, _, _) in site_tables() {
            let qr = store.execute_query(&format!(
                "MATCH (c:CallSite)-[r:{rel}]->() WHERE c.id IN [{list}]{filter} RETURN c.id"
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
