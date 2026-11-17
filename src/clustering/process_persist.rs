// clustering::process_persist — the writes of `trace_processes`.
//
// The processes are accumulated in memory and written with one bulk call per
// table, after every process has been traced: the `Process` nodes first, then
// the `EntryPointOf_*` and `ParticipatesIn_*` edges, which MATCH them. The
// rows are the ones the per-process `CREATE` and `MATCH ... CREATE` used to
// write, one statement per process (issue #443).

use super::EntryPoint;
use crate::graph_store::{cypher_str, is_known_rel_table, GraphStore, PropEdgeList};
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

    /// Nodes first: every edge MATCHes its `Process` endpoint.
    pub(super) fn write(self, store: &GraphStore) -> Result<(), String> {
        store.bulk_insert_nodes("Process", &self.nodes)?;
        for (rel, edges) in &self.edges {
            store.bulk_insert_edges(rel, edges)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "process_persist_tests.rs"]
mod tests;
