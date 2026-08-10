// clustering::community_persist — writes a computed partition back to the
// graph as Community nodes + MemberOf_<Label>_Community edges.
// source: stages/stage-3c.md §4.
//
// Extracted from community.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// clustering vocabulary exactly as when this lived in one module.

use super::*;

/// Remove Community and Process nodes (and their edges) left by a prior
/// clustering pass. Both node tables use `id` as primary key, so re-running
/// `cluster_graph` on an already-clustered graph would otherwise abort with
/// a duplicate-primary-key error instead of re-clustering (bench q12 scored
/// 0.000 because the harness clusters once at setup and once per label).
pub(super) fn purge_prior_clustering(store: &GraphStore) -> Result<(), String> {
    for label in ["Community", "Process"] {
        store
            .execute_query(&format!("MATCH (n:{label}) DETACH DELETE n"))
            .map_err(|e| format!("purge {label}: {e}"))?;
    }
    Ok(())
}

pub(super) fn persist_communities(
    store: &GraphStore,
    adj: &Adjacency,
    comm: &[usize],
    modularity: f64,
    gamma: f64,
) -> Result<u64, String> {
    let num_comms = comm.iter().copied().max().map_or(0, |m| m + 1);

    // Count members per community
    let mut counts: HashMap<usize, u64> = HashMap::new();
    for &c in comm {
        *counts.entry(c).or_insert(0) += 1;
    }

    // Create Community nodes (bulk-insert).
    // source: Fermi audit April 2026 — was per-row CREATE, now batched.
    let community_rows: Vec<Vec<(String, String)>> = (0..num_comms)
        .map(|c| {
            let count = counts.get(&c).copied().unwrap_or(0);
            let cid = format!("community::louvain::{gamma}::{c}");
            vec![
                ("id".into(), cypher_str(&cid)),
                ("name".into(), format!("'community_{c}'")),
                ("algorithm".into(), "'louvain+c2'".into()),
                ("resolution_param".into(), gamma.to_string()),
                ("member_count".into(), count.to_string()),
                (
                    "modularity_contribution".into(),
                    format!("{:.6}", modularity),
                ),
            ]
        })
        .collect();
    store.bulk_insert_nodes("Community", &community_rows)?;

    // Create MemberOf edges grouped per rel table. Only labels with a
    // MemberOf_<Label>_Community table are persisted — File nodes take part
    // in the Louvain adjacency as containment carriers but have no
    // membership table (and q12 scoring keys on symbol qualified names).
    let mut by_rel: HashMap<String, PropEdgeList> = HashMap::new();
    for (idx, &c) in comm.iter().enumerate() {
        let node_id = &adj.node_ids[idx];
        let label = &adj.node_labels[idx];
        if !MEMBEROF_LABELS.contains(&label.as_str()) {
            continue;
        }
        let cid = format!("community::louvain::{gamma}::{c}");
        let rel = format!("MemberOf_{label}_Community");
        by_rel
            .entry(rel)
            .or_default()
            .push((node_id.clone(), cid, Vec::new()));
    }
    for (rel, edges) in &by_rel {
        store.bulk_insert_edges(rel, edges)?;
    }
    Ok(num_comms as u64)
}
