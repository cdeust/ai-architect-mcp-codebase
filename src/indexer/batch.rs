// indexer::batch — cross-file symbol accumulation for bulk insertion.
//
// Extracted from indexer/mod.rs (Fowler "Extract Class") to keep the pipeline
// entry point under the §4.1 file cap. Pure move: no behavior change.

use crate::graph_store::{GraphStore, PropEdgeList};
use std::collections::HashMap;

// Flush the accumulated symbol batch once it holds this many node rows.
// source: measured April 2026 (Fermi scalability audit) — bulk-inserting
// symbols PER FILE issued ~15 small bulk calls per file (one per label/edge
// table), each paying prepare-lookup + FFI round-trip overhead; at 500 files
// that was ~131 s of the 140 s indexing time. Accumulating across files and
// flushing in large batches turns ~7500 small calls into a few dozen large
// ones. 5000 rows bounds peak batch memory (~1-2 MB) while fully amortizing
// the per-call overhead; the existing BULK_BATCH_SIZE (500) still chunks each
// bulk call internally.
pub(super) const SYMBOL_FLUSH_THRESHOLD: usize = 5_000;

/// Accumulates parsed nodes and edges across many files so they can be
/// bulk-inserted in large batches instead of one small bulk call per file.
///
/// Safe because every edge the indexer emits is intra-file (Defines/HasMethod/
/// HasField/HasVariant, File→symbol) — there are no cross-file edges at index
/// time (Calls/Uses are resolved later). On flush, all nodes are inserted
/// before any edge, so every edge finds its endpoints. File/Directory nodes
/// (inserted eagerly per file) already exist when the symbol batch flushes.
#[derive(Default)]
pub(super) struct SymbolBatch {
    nodes: HashMap<String, Vec<Vec<(String, String)>>>,
    edges: HashMap<String, PropEdgeList>,
    pub(super) node_row_count: usize,
}

impl SymbolBatch {
    pub(super) fn push_node(&mut self, label: &str, row: Vec<(String, String)>) {
        self.nodes.entry(label.to_string()).or_default().push(row);
        self.node_row_count += 1;
    }

    pub(super) fn push_edge(
        &mut self,
        table: &str,
        from: String,
        to: String,
        props: Vec<(String, String)>,
    ) {
        self.edges
            .entry(table.to_string())
            .or_default()
            .push((from, to, props));
    }

    /// Inserts every accumulated node (all labels) and THEN every accumulated
    /// edge (all tables), so edges always resolve their endpoints. Empties
    /// the batch.
    pub(super) fn flush(&mut self, store: &GraphStore) -> Result<(), String> {
        for (label, rows) in self.nodes.drain() {
            store.bulk_insert_nodes(&label, &rows)?;
        }
        for (table, edges) in self.edges.drain() {
            store.bulk_insert_edges(&table, &edges)?;
        }
        self.node_row_count = 0;
        Ok(())
    }
}
