// clustering::process::process_persist — the writes of `trace_processes`.
//
// The processes are accumulated in memory and written in bulk calls: the
// `Process` nodes first, then the `EntryPointOf_*` and `ParticipatesIn_*`
// edges, which MATCH them. The rows are the ones the per-process `CREATE` and
// `MATCH ... CREATE` used to write, one statement per process (issue #443).
//
// Memory is bounded: after each process, `flush_full_chunks` writes every
// table that holds at least one full `BULK_BATCH_SIZE` chunk, and keeps only
// the remainder. Right after a flush, fewer than `BULK_BATCH_SIZE` rows are
// pending per table; the peak adds to that the rows of the one process being
// traced, which `add_participants` pushes before the next flush. Holding every
// row until the end made the peak grow as entries x symbols reached: O(N^2)
// whenever N entries each reach O(N) symbols, for example N public functions
// that all reach one hub which itself reaches O(N) symbols (review of #561).
//
// Calls: the edge tables cost the same bulk calls as one final write, since
// only full chunks are written early. The `Process` table costs one call per
// early flush plus the final one, because the nodes of the pending edges must
// exist before those edges MATCH them. Early flushes are not bounded by
// ceil(N / BULK_BATCH_SIZE): the trigger counts per table, so when every
// process adds at least `BULK_BATCH_SIZE` edges to one table, every process
// flushes, and `Process` costs up to one call per entry. Each early flush writes at least one full edge chunk or one
// full chunk of nodes, so the `Process` calls stay at most the edge chunks
// written early, plus floor(N / BULK_BATCH_SIZE), plus the final call
// (review of #563).

use super::EntryPoint;
use crate::graph_store::{
    cypher_str, is_known_rel_table, GraphStore, PropEdgeList, BULK_BATCH_SIZE,
};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct ProcessBatch {
    nodes: Vec<Vec<(String, String)>>,
    edges: HashMap<String, PropEdgeList>,
}

impl ProcessBatch {
    /// One `Process` node and, for a callable entry, its `EntryPointOf` edge.
    pub(super) fn add_process(
        &mut self,
        entry: &EntryPoint,
        process_id: &str,
        symbol_count: u64,
        max_depth: u64,
    ) {
        let proc_lit = cypher_str(process_id);
        self.nodes.push(vec![
            ("id".into(), proc_lit.clone()),
            ("name".into(), proc_lit),
            ("entry_point_id".into(), cypher_str(&entry.id)),
            ("entry_kind".into(), cypher_str(&entry.kind)),
            ("entry_confidence".into(), entry.confidence.to_string()),
            ("depth".into(), max_depth.to_string()),
            ("symbol_count".into(), symbol_count.to_string()),
        ]);
        let rel = format!("EntryPointOf_{}_Process", entry.label);
        // EntryPointOf tables only defined for Function/Method per REL_TABLES.
        if !is_known_rel_table(&rel) {
            eprintln!(
                "clustering: skipping EntryPointOf for non-callable label {}",
                entry.label
            );
            return;
        }
        self.edges.entry(rel).or_default().push((
            entry.id.clone(),
            process_id.to_string(),
            vec![("confidence".into(), entry.confidence.to_string())],
        ));
    }

    /// One `ParticipatesIn` edge per node the BFS reached, carrying its depth.
    pub(super) fn add_participants(
        &mut self,
        depths: &HashMap<String, u64>,
        process_id: &str,
        id_to_label: &HashMap<String, String>,
    ) {
        for (node_id, depth) in depths {
            let Some(lbl) = id_to_label.get(node_id) else {
                continue;
            };
            let rel = format!("ParticipatesIn_{lbl}_Process");
            // ParticipatesIn tables only defined for Function/Method per
            // REL_TABLES. Filter at producer side instead of swallowing the
            // bulk_insert error (which previously dropped real edges too).
            if !is_known_rel_table(&rel) {
                continue;
            }
            // source: ordering fix — the real BFS depth (step number in the
            // process's call chain), not a hardcoded 0, so a consumer can
            // order participants by it.
            self.edges.entry(rel).or_default().push((
                node_id.clone(),
                process_id.to_string(),
                vec![("depth".into(), depth.to_string())],
            ));
        }
    }

    /// Writes the full `BULK_BATCH_SIZE` chunks of every edge table, and
    /// keeps the remainder. Called between two processes, so every pending
    /// edge's `Process` node is pending or already written: the pending nodes
    /// are written first. With no full edge chunk, the nodes are written only
    /// once they fill a chunk themselves. In `trace_processes` that second
    /// trigger never fires alone, since every entry there is a `Function`
    /// with an `EntryPointOf` edge, so that table holds at least as many
    /// pending rows as the nodes; it bounds the nodes of a batch whose
    /// entries have no `EntryPointOf` table.
    pub(super) fn flush_full_chunks(&mut self, store: &GraphStore) -> Result<(), String> {
        let full_edges = self.edges.values().any(|e| e.len() >= BULK_BATCH_SIZE);
        if !full_edges && self.nodes.len() < BULK_BATCH_SIZE {
            return Ok(());
        }
        self.write_nodes(store)?;
        for (rel, edges) in &mut self.edges {
            let full = edges.len() - edges.len() % BULK_BATCH_SIZE;
            if full == 0 {
                continue;
            }
            store.bulk_insert_edges(rel, &edges[..full])?;
            edges.drain(..full);
        }
        Ok(())
    }

    /// Nodes first: every edge MATCHes its `Process` endpoint.
    pub(super) fn write(mut self, store: &GraphStore) -> Result<(), String> {
        self.write_nodes(store)?;
        for (rel, edges) in &self.edges {
            store.bulk_insert_edges(rel, edges)?;
        }
        Ok(())
    }

    fn write_nodes(&mut self, store: &GraphStore) -> Result<(), String> {
        store.bulk_insert_nodes("Process", &self.nodes)?;
        self.nodes.clear();
        Ok(())
    }

    /// The rows not yet written: `Process` nodes, then each edge table by
    /// name, sorted.
    #[cfg(test)]
    pub(super) fn pending(&self) -> (usize, Vec<(String, usize)>) {
        let mut tables: Vec<(String, usize)> = self
            .edges
            .iter()
            .map(|(rel, e)| (rel.clone(), e.len()))
            .collect();
        tables.sort();
        (self.nodes.len(), tables)
    }
}

#[cfg(test)]
#[path = "process_persist_tests.rs"]
mod tests;
