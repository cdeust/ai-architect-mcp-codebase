// resolver::edge_buffer — EdgeBuffer, the in-memory staging area for
// resolution edges (moved out of resolver/mod.rs to keep it under the §4.1
// cap; issue #443 adds the decision of each row the pass meets again).
//
// Collects all resolved edges across phases, deduplicates via a HashSet of
// (rel_table, from, to) triples, and flushes grouped by rel_table through
// GraphStore::bulk_insert_edges at the end. Eliminates per-edge MATCH+CREATE
// round-trips and the idempotency sub-query that used to run before every
// insert_resolution_edge call.
// source: Fermi audit April 2026 — resolver was bottlenecked by this loop.

use super::retract::{Decision, Met};
use super::AddOutcome;
use crate::graph_store::{GraphStore, PropEdgeList};
use std::collections::{HashMap, HashSet};

pub struct EdgeBuffer {
    pub(super) by_table: HashMap<String, PropEdgeList>,
    /// Edges that existed in the store before this run started. Read-only
    /// after construction — never mutated by `add`.
    persisted: HashSet<(String, String, String)>,
    /// Edges staged during this run (superset check happens against
    /// `persisted` first, so this set never overlaps it).
    staged: HashSet<(String, String, String)>,
    /// The `persisted` edges this run decided again (`AlreadyPersisted`),
    /// each with the first decision the run made for it: the confidence and
    /// method a fresh index would have written, since there that first `add`
    /// is the `Inserted` one. A persisted row of the static tiers outside this
    /// map is retracted after the flush, and one inside it whose properties
    /// differ takes the decision (issue #443, `retract` and `retract_edges`).
    met: Met,
}

impl EdgeBuffer {
    pub(super) fn new(existing: HashSet<(String, String, String)>) -> Self {
        Self {
            by_table: HashMap::new(),
            persisted: existing,
            staged: HashSet::new(),
            met: Met::new(),
        }
    }

    /// Stages an edge for the given (rel_table, from, to) key.
    ///
    /// precondition: `rel_table` is a schema-known relationship table (the
    /// caller has already checked `check_known_rel_table`).
    /// postcondition: returns the outcome (see `AddOutcome`); on
    /// `Inserted`, the edge is queued in `by_table` for `flush`. On
    /// `AlreadyPersisted` / `DuplicateInRun`, no new row is queued — the
    /// edge is either already in the store or already staged this run.
    pub fn add(
        &mut self,
        rel_table: &str,
        from_id: &str,
        to_id: &str,
        confidence: f64,
        method: &str,
    ) -> AddOutcome {
        let key = (
            rel_table.to_string(),
            from_id.to_string(),
            to_id.to_string(),
        );
        if self.persisted.contains(&key) {
            self.met.entry(key).or_insert_with(|| Decision {
                confidence,
                method: method.to_string(),
            });
            return AddOutcome::AlreadyPersisted;
        }
        if self.staged.contains(&key) {
            return AddOutcome::DuplicateInRun;
        }
        self.staged.insert(key);
        let props = vec![
            ("confidence".to_string(), confidence.to_string()),
            ("resolution_method".to_string(), format!("'{method}'")),
        ];
        self.by_table
            .entry(rel_table.to_string())
            .or_default()
            .push((from_id.to_string(), to_id.to_string(), props));
        AddOutcome::Inserted
    }

    /// The rows the run met again, with their decisions, emptied from the
    /// buffer (read before `flush`, which consumes it).
    pub(super) fn take_met(&mut self) -> Met {
        std::mem::take(&mut self.met)
    }

    pub(super) fn flush(self, store: &GraphStore) -> Result<(), String> {
        // Tolerate unknown rel tables: the resolver builds rel-table
        // names dynamically from caller+target labels (e.g.
        // Calls_Method_Struct when a method calls a struct constructor),
        // but REL_TABLES only declares a subset of label pairs. Skipping
        // the unknown ones with a warning lets every valid table still
        // flush. Previously, one unknown table aborted the whole flush
        // via ``?`` and dropped every resolved edge — which is why
        // downstream consumers (Cortex viz) saw zero Calls/Imports rows.
        for (table, edges) in &self.by_table {
            match store.bulk_insert_edges(table, edges) {
                Ok(_) => {}
                Err(e) if e.contains("unknown relationship type") => {
                    eprintln!(
                        "resolver: skipping {} edges on unknown rel table {} ({})",
                        edges.len(),
                        table,
                        e,
                    );
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
