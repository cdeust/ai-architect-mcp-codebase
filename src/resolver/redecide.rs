// resolver::redecide: the provenance of a row a pass decides again (issue
// #443, Claude review of #523).
//
// A row an earlier pass wrote and this pass decides again is not rewritten:
// `EdgeBuffer::add` answers `AlreadyPersisted` and stages nothing, so the row
// kept the `confidence` and `resolution_method` of the pass that wrote it. A
// full index of the same tree writes the row once, with the first decision of
// its single pass. The two differ as soon as an edit changes the evidence
// without changing the target: `a.py` defines and calls `beta`, unique in the
// first pass (`unique-match`, 0.95); an edit adds a second `beta` in `c.py`,
// the next pass resolves the same call to the same `a.py::beta` as the one in
// the caller's file (`same-file-unique`, 0.85), and the refreshed rows still
// read `unique-match` with 0.95.
//
// After the flush, each row the pass met again (`EdgeBuffer::take_met`, with
// the first decision of the pass for it) whose owned row (`Retraction::held`,
// read before the pass) holds another confidence or method takes the
// decision. Only rows the retraction owns are touched, so a row of another
// writer (language server, macro layer, light links, IaC) keeps its own life
// cycle, and the update sets only the two provenance columns, so trace
// ingest's `observed_count` stays.

use super::retract::{Decision, Met, RowKey};
use crate::graph_store::{cypher_str, GraphStore, REL_TABLES};
use std::collections::HashMap;

/// Two confidences closer than this are the same value read back through the
/// store's text rendering of a DOUBLE.
const CONFIDENCE_EPSILON: f64 = 1e-9;

/// Whether `held` already carries `decided`: same method, same confidence.
/// A NaN confidence (absent in the store) carries no decision.
fn carries(held: &Decision, decided: &Decision) -> bool {
    held.method == decided.method
        && (held.confidence - decided.confidence).abs() <= CONFIDENCE_EPSILON
}

/// The owned rows of `met` whose held provenance differs from the decision,
/// sorted so the updates run in a stable order.
fn changed<'a>(
    held: &'a HashMap<RowKey, Decision>,
    met: &'a Met,
) -> Vec<(&'a RowKey, &'a Decision, &'a Decision)> {
    let mut rows: Vec<_> = met
        .iter()
        .filter_map(|(key, decided)| {
            let was = held.get(key)?;
            (!carries(was, decided)).then_some((key, was, decided))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    rows
}

/// Gives each owned row of `met` whose provenance differs the confidence and
/// method the pass decided. Returns the number of rows updated.
///
/// Precondition: `held` was read by `Retraction::begin` before the pass, and
/// the pass's buffer has been flushed. Idempotent: a second call with the same
/// arguments rewrites the same values. The `WHERE` on the held method keeps
/// the update off a row of another writer between the same two nodes.
pub(super) fn apply(
    store: &GraphStore,
    held: &HashMap<RowKey, Decision>,
    met: &Met,
) -> Result<u64, String> {
    let mut updated = 0u64;
    for ((rel, from_id, to_id), was, decided) in changed(held, met) {
        let Some(&(_, from, to)) = REL_TABLES.iter().find(|(t, _, _)| t == rel) else {
            continue;
        };
        store.execute_query(&format!(
            "MATCH (a:{from})-[r:{rel}]->(b:{to}) WHERE a.id = {} AND b.id = {} \
             AND r.resolution_method = {} SET r.confidence = {:?}, r.resolution_method = {}",
            cypher_str(from_id),
            cypher_str(to_id),
            cypher_str(&was.method),
            decided.confidence,
            cypher_str(&decided.method),
        ))?;
        updated += 1;
    }
    Ok(updated)
}

#[cfg(test)]
#[path = "redecide_tests.rs"]
mod tests;
