use crate::graph_store::{cypher_str, GraphStore, PropEdgeList};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use super::process::trace_processes;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct ClusteringResult {
    pub communities: u64,
    pub modularity: f64,
    pub processes: u64,
    pub elapsed_ms: u64,
}

/// One (symbol → community) membership row, derived from the persisted
/// MemberOf_<Label>_Community edge tables.
pub struct ClusterMembership {
    pub qualified_name: String,
    pub community_id: String,
    pub cluster_id: i64,
}

/// Bounded view over the full membership mapping. `total` is the pre-cap
/// count; when `truncated_at` is `Some(n)`, `entries.len() == n < total`
/// and the remainder is still reachable via `query_graph`.
pub struct ClusterMemberships {
    pub entries: Vec<ClusterMembership>,
    pub truncated_at: Option<usize>,
    pub total: usize,
}

const CLUSTERS_RESPONSE_CAP: usize = 10_000;

const MEMBEROF_LABELS: &[&str] = &[
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Trait",
    "Constant",
    "TypeAlias",
    "Module",
];

/// Collect per-symbol community memberships by scanning every
/// `MemberOf_<Label>_Community` edge table. The response is capped at
/// `CLUSTERS_RESPONSE_CAP` entries; the full mapping remains queryable via
/// `query_graph` against the same edge tables.
pub fn collect_cluster_memberships(store: &GraphStore) -> Result<ClusterMemberships, String> {
    let mut entries: Vec<ClusterMembership> = Vec::new();
    for label in MEMBEROF_LABELS {
        let rel = format!("MemberOf_{label}_Community");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(c:Community) \
             RETURN n.qualified_name, c.id"
        );
        let qr = match store.execute_query(&cypher) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in qr.rows {
            if row.len() < 2 {
                continue;
            }
            let cid = cluster_id_from_community_id(&row[1]);
            entries.push(ClusterMembership {
                qualified_name: row[0].clone(),
                community_id: row[1].clone(),
                cluster_id: cid,
            });
        }
    }
    Ok(sort_and_cap_memberships(entries))
}

/// Sort entries deterministically before applying the 10k truncation cap.
/// Must-fix from d-review.md §6: lbug/Kuzu row order per query is not
/// guaranteed, so an unsorted truncation would drop arbitrary entries per
/// run and break Q12 ARI reproducibility on graphs exceeding the cap.
fn sort_and_cap_memberships(mut entries: Vec<ClusterMembership>) -> ClusterMemberships {
    entries.sort_by(|a, b| {
        a.qualified_name
            .cmp(&b.qualified_name)
            .then_with(|| a.community_id.cmp(&b.community_id))
    });
    let total = entries.len();
    let truncated_at = if total > CLUSTERS_RESPONSE_CAP {
        entries.truncate(CLUSTERS_RESPONSE_CAP);
        Some(CLUSTERS_RESPONSE_CAP)
    } else {
        None
    };
    ClusterMemberships {
        entries,
        truncated_at,
        total,
    }
}

/// community_id persisted by `persist_communities` is
/// `community::louvain::<gamma>::<N>`. Extract the trailing integer so
/// the bench harness (which scores clusters via adjusted Rand index on
/// integer labels) can map community ids without parsing the prefix.
pub fn cluster_id_from_community_id(community_id: &str) -> i64 {
    community_id
        .rsplit("::")
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(-1)
}

// ---------------------------------------------------------------------------
// Edge weight table — source: stages/stage-3c.md §2.4
// ---------------------------------------------------------------------------

fn edge_weight(rel_name: &str) -> f64 {
    if rel_name.starts_with("Calls_") {
        3.0
    } else if rel_name.starts_with("Implements_") || rel_name.starts_with("Extends_") {
        2.0
    } else if rel_name.starts_with("Imports_") || rel_name.starts_with("Uses_") {
        1.0
    } else if rel_name.starts_with("HasMethod_")
        || rel_name.starts_with("HasField_")
        || rel_name.starts_with("HasVariant_")
        || rel_name.starts_with("Defines_")
        || rel_name.starts_with("Contains_")
    {
        5.0
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Adjacency extraction — source: stages/stage-3c.md §2.4
// ---------------------------------------------------------------------------

/// Collected cluster-eligible node identity: (ids, labels, id-to-adjacency-
/// index map), all index-aligned. clippy::type_complexity — named so the
/// shape is documented once instead of repeated at every call site.
type SymbolNodeCollection = (Vec<String>, Vec<String>, HashMap<String, usize>);

/// Per-node weighted neighbor lists (adjacency[i] = [(neighbor_idx, weight)])
/// plus the total edge weight summed across the whole adjacency.
/// clippy::type_complexity.
type WeightedAdjacency = (Vec<Vec<(usize, f64)>>, f64);

struct Adjacency {
    node_ids: Vec<String>,
    node_labels: Vec<String>,
    #[allow(dead_code)] // used by tests for constructing test adjacencies
    id_to_idx: HashMap<String, usize>,
    neighbors: Vec<Vec<(usize, f64)>>,
    total_weight: f64,
}

/// Node labels fed to Louvain: the clusterable symbols plus File nodes.
/// File nodes act as containment carriers — Defines_File_* edges bind
/// same-file symbols, which is the dominant module-affinity signal (measured
/// on the rust-self bench graph: ~1129 Defines_File_* edges vs ~1559 Calls;
/// without them the adjacency has no file/module co-membership signal at all
/// and the partition cannot recover the directory-based ground truth).
/// Directory nodes stay excluded (stage-3c.md §2.4). File memberships are
/// NOT persisted — see `persist_communities`.
const CLUSTER_NODE_LABELS: &[&str] = &[
    "File",
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Trait",
    "Constant",
    "TypeAlias",
    "Module",
];

fn extract_adjacency(store: &GraphStore) -> Result<Adjacency, String> {
    let (node_ids, node_labels, id_to_idx) = collect_symbol_nodes(store)?;
    let n = node_ids.len();
    let (neighbors, total_weight) = collect_weighted_edges(store, &id_to_idx, n)?;
    Ok(Adjacency {
        node_ids,
        node_labels,
        id_to_idx,
        neighbors,
        total_weight,
    })
}

fn collect_symbol_nodes(store: &GraphStore) -> Result<SymbolNodeCollection, String> {
    let mut ids = Vec::new();
    let mut labels = Vec::new();
    let mut map: HashMap<String, usize> = HashMap::new();
    for label in CLUSTER_NODE_LABELS {
        // ORDER BY: without it, Kuzu's row order for an unordered MATCH is
        // an internal storage-scan detail, not a stability guarantee across
        // query executions. This function assigns each id its adjacency
        // INDEX by push order, so an unordered scan makes node index
        // assignment (and everything downstream: `one_level`'s `for i in
        // 0..n` processing order, and float-summation order in `ki_in`)
        // non-deterministic run-to-run even for byte-identical graph
        // content — invisible on a small graph (few near-tie modularity
        // decisions), increasingly visible as the graph grows (more
        // decision points where a different accumulation order flips a
        // `gain > best_gain` comparison by float-rounding noise). Root
        // cause of a real bug, not a tolerance to loosen: two identical
        // `cluster_graph` calls on an unchanged graph must return the same
        // partition (Q12/Q13 ARI reproducibility requirement).
        let cypher = format!("MATCH (n:{label}) RETURN n.id ORDER BY n.id");
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.is_empty() {
                continue;
            }
            let id = &row[0];
            if !map.contains_key(id) {
                map.insert(id.clone(), ids.len());
                ids.push(id.clone());
                labels.push(label.to_string());
            }
        }
    }
    Ok((ids, labels, map))
}

fn collect_weighted_edges(
    store: &GraphStore,
    id_to_idx: &HashMap<String, usize>,
    n: usize,
) -> Result<WeightedAdjacency, String> {
    let mut neighbors: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut total_weight = 0.0;
    for &(rel, from_label, to_label) in edge_rel_tables() {
        let w = edge_weight(rel);
        if w == 0.0 {
            continue;
        }
        // ORDER BY: same determinism requirement as `collect_symbol_nodes`
        // above — each `neighbors[i]` push happens in query-row order, and
        // `ki_in`'s per-node weight accumulation (`one_level`) sums in that
        // same order; floating-point addition is not strictly associative,
        // so an unordered scan can flip a near-tie `gain > best_gain`
        // comparison between otherwise-identical runs.
        let cypher = format!(
            "MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) \
             RETURN a.id, b.id ORDER BY a.id, b.id"
        );
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 2 {
                continue;
            }
            if let (Some(&a), Some(&b)) = (id_to_idx.get(&row[0]), id_to_idx.get(&row[1])) {
                neighbors[a].push((b, w));
                neighbors[b].push((a, w));
                total_weight += w;
            }
        }
    }
    Ok((neighbors, total_weight))
}

fn edge_rel_tables() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        ("Defines_File_Function", "File", "Function"),
        ("Defines_File_Struct", "File", "Struct"),
        ("Defines_File_Enum", "File", "Enum"),
        ("Defines_File_Trait", "File", "Trait"),
        ("Defines_File_Constant", "File", "Constant"),
        ("Defines_File_TypeAlias", "File", "TypeAlias"),
        ("Defines_Module_Function", "Module", "Function"),
        ("Defines_Module_Struct", "Module", "Struct"),
        ("Defines_Module_Enum", "Module", "Enum"),
        ("Defines_Module_Trait", "Module", "Trait"),
        ("Defines_Module_Constant", "Module", "Constant"),
        ("Defines_Module_TypeAlias", "Module", "TypeAlias"),
        ("Contains_File_Module", "File", "Module"),
        ("Imports_File_File", "File", "File"),
        ("Imports_File_Module", "File", "Module"),
        ("Calls_Function_Function", "Function", "Function"),
        ("Calls_Function_Method", "Function", "Method"),
        ("Calls_Method_Function", "Method", "Function"),
        ("Calls_Method_Method", "Method", "Method"),
        ("Imports_File_Function", "File", "Function"),
        ("Imports_File_Struct", "File", "Struct"),
        ("Imports_File_Enum", "File", "Enum"),
        ("Imports_File_Trait", "File", "Trait"),
        ("Implements_Struct_Trait", "Struct", "Trait"),
        ("Implements_Enum_Trait", "Enum", "Trait"),
        ("Extends_Trait_Trait", "Trait", "Trait"),
        ("Uses_Function_Struct", "Function", "Struct"),
        ("Uses_Function_Enum", "Function", "Enum"),
        ("Uses_Function_Trait", "Function", "Trait"),
        ("Uses_Method_Struct", "Method", "Struct"),
        ("Uses_Method_Enum", "Method", "Enum"),
        ("Uses_Method_Trait", "Method", "Trait"),
        ("HasMethod_Struct_Method", "Struct", "Method"),
        ("HasMethod_Enum_Method", "Enum", "Method"),
        ("HasMethod_Trait_Method", "Trait", "Method"),
        ("HasField_Struct_Field", "Struct", "Field"),
        ("HasField_Enum_Field", "Enum", "Field"),
        ("HasVariant_Enum_Variant", "Enum", "Variant"),
    ]
}

// ---------------------------------------------------------------------------
// Louvain algorithm — Blondel et al. 2008
// source: "Fast unfolding of communities in large networks"
// ---------------------------------------------------------------------------

/// Blondel et al. 2008, section II: "the two phases are repeated
/// iteratively". Phase 1 (`one_level`) moves individual nodes; phase 2
/// (`aggregate_graph`) condenses each community into a super-node so whole
/// communities can merge on the next pass. Iterating until a pass produces
/// no merge is what lets the partition reach coarse module granularity —
/// phase 1 alone leaves the graph over-fragmented (measured on the rust-self
/// bench corpus: 1262 communities for 2567 symbols without phase 2).
fn louvain(adj: &Adjacency, gamma: f64) -> (Vec<usize>, f64) {
    let n = adj.node_ids.len();
    if n == 0 {
        return (vec![], 0.0);
    }
    let m = adj.total_weight; // sum of edge weights (each undirected edge once)
    if m == 0.0 {
        return ((0..n).collect(), 0.0);
    }

    // node_to_comm[i] = community of ORIGINAL node i, refined level by level.
    let mut node_to_comm: Vec<usize> = (0..n).collect();
    let mut level_neighbors: Vec<Vec<(usize, f64)>> = adj.neighbors.clone();

    loop {
        let level_n = level_neighbors.len();
        let comm = one_level(&level_neighbors, m, gamma);
        let num_comms = comm.iter().copied().max().map_or(0, |c| c + 1);
        for c in node_to_comm.iter_mut() {
            *c = comm[*c];
        }
        if num_comms == level_n {
            break; // no merge at this level — partition is stable
        }
        level_neighbors = aggregate_graph(&level_neighbors, &comm, num_comms);
    }

    let k: Vec<f64> = adj
        .neighbors
        .iter()
        .map(|nbrs| nbrs.iter().map(|(_, w)| w).sum())
        .collect();
    let q = compute_modularity(&adj.neighbors, &node_to_comm, &k, m);
    (node_to_comm, q)
}

/// Phase 1 of Blondel 2008: greedy local node moves until no move improves
/// modularity. `m` is the total edge weight of the ORIGINAL graph — it is
/// invariant under aggregation and must not be recomputed per level.
fn one_level(neighbors: &[Vec<(usize, f64)>], m: f64, gamma: f64) -> Vec<usize> {
    let n = neighbors.len();
    let two_m = 2.0 * m; // Newman's 2m: sum of degrees = 2 * sum of edge weights

    // k[i] = sum of incident weights (self-loops stored with their full
    // degree contribution by aggregate_graph, so a plain sum is correct).
    let k: Vec<f64> = neighbors
        .iter()
        .map(|nbrs| nbrs.iter().map(|(_, w)| w).sum())
        .collect();

    let mut comm: Vec<usize> = (0..n).collect();
    // sigma_tot[c] = sum of degrees of nodes in community c
    let mut sigma_tot: Vec<f64> = k.clone();
    let max_passes = 100;

    for _ in 0..max_passes {
        let mut improved = false;
        for i in 0..n {
            let old_c = comm[i];
            let ki = k[i];

            // Weights from i to each neighboring community. Self-loops are
            // skipped: they stay internal wherever i moves, so counting them
            // in ki_in[old_c] would bias every node toward never moving.
            let mut ki_in: HashMap<usize, f64> = HashMap::new();
            for &(nbr, w) in &neighbors[i] {
                if nbr == i {
                    continue;
                }
                *ki_in.entry(comm[nbr]).or_insert(0.0) += w;
            }

            // Remove i from its community for gain computation
            sigma_tot[old_c] -= ki;

            // Gain = ki_in_c - gamma * sigma_tot_c * ki / (2m)
            // source: Blondel 2008 eq. from section III
            let ki_in_old = ki_in.get(&old_c).copied().unwrap_or(0.0);
            let mut best_c = old_c;
            let mut best_gain = ki_in_old - gamma * sigma_tot[old_c] * ki / two_m;

            // Sorted candidate order: HashMap iteration is nondeterministic,
            // and a tie on gain must resolve identically across runs (Q12 ARI
            // reproducibility requirement, d-review.md §6).
            let mut candidates: Vec<(usize, f64)> = ki_in.iter().map(|(&c, &w)| (c, w)).collect();
            candidates.sort_unstable_by_key(|&(c, _)| c);
            for &(c, ki_in_c) in &candidates {
                let gain = ki_in_c - gamma * sigma_tot[c] * ki / two_m;
                if gain > best_gain {
                    best_gain = gain;
                    best_c = c;
                }
            }

            comm[i] = best_c;
            sigma_tot[best_c] += ki;
            if best_c != old_c {
                improved = true;
            }
        }
        if !improved {
            break;
        }
    }

    renumber_communities(&comm)
}

/// Phase 2 of Blondel 2008: build the condensed graph whose nodes are the
/// communities of the previous level. Cross-community weights sum once per
/// direction (the input stores each undirected edge twice), so the result
/// keeps the same both-directions convention. Intra-community weight lands
/// on a single self-loop entry carrying the full degree contribution
/// (2 × internal weight), preserving node degrees and total weight m.
fn aggregate_graph(
    neighbors: &[Vec<(usize, f64)>],
    comm: &[usize],
    num_comms: usize,
) -> Vec<Vec<(usize, f64)>> {
    let mut weights: Vec<HashMap<usize, f64>> = vec![HashMap::new(); num_comms];
    for (i, nbrs) in neighbors.iter().enumerate() {
        let ci = comm[i];
        for &(j, w) in nbrs {
            *weights[ci].entry(comm[j]).or_insert(0.0) += w;
        }
    }
    weights
        .into_iter()
        .map(|w| {
            let mut entries: Vec<(usize, f64)> = w.into_iter().collect();
            entries.sort_unstable_by_key(|&(c, _)| c); // determinism
            entries
        })
        .collect()
}

fn renumber_communities(comm: &[usize]) -> Vec<usize> {
    let mut map: HashMap<usize, usize> = HashMap::new();
    let mut next = 0;
    let mut result = Vec::with_capacity(comm.len());
    for &c in comm {
        let new_c = *map.entry(c).or_insert_with(|| {
            let v = next;
            next += 1;
            v
        });
        result.push(new_c);
    }
    result
}

/// Newman 2004: Q = (1/2m) * sum_ij [A_ij - ki*kj/(2m)] * delta(ci,cj)
/// `m` = sum of undirected edge weights (each edge counted once).
fn compute_modularity(neighbors: &[Vec<(usize, f64)>], comm: &[usize], k: &[f64], m: f64) -> f64 {
    if m == 0.0 {
        return 0.0;
    }
    let two_m = 2.0 * m;
    let mut q = 0.0;
    // neighbors stores both directions, so the loop sums each pair (i,j) twice.
    // This cancels with the 1/(2m) factor, leaving division by two_m once.
    for (i, nbrs) in neighbors.iter().enumerate() {
        for &(j, w) in nbrs {
            if comm[i] == comm[j] {
                q += w - k[i] * k[j] / two_m;
            }
        }
    }
    q / two_m
}

// ---------------------------------------------------------------------------
// C2 repair: split disconnected communities — Traag 2019 §3.2
// ---------------------------------------------------------------------------

fn repair_c2(adj: &Adjacency, comm: &mut Vec<usize>) {
    let n = comm.len();
    let num_comms = comm.iter().copied().max().map_or(0, |m| m + 1);
    let mut next_comm = num_comms;

    for c in 0..num_comms {
        let members: Vec<usize> = (0..n).filter(|&i| comm[i] == c).collect();
        if members.len() <= 1 {
            continue;
        }

        let components = connected_components_within(&members, &adj.neighbors, comm, c);
        if components.len() <= 1 {
            continue;
        }

        // Keep first component as c, assign rest new IDs
        for component in components.iter().skip(1) {
            for &node in component {
                comm[node] = next_comm;
            }
            next_comm += 1;
        }
    }
    *comm = renumber_communities(comm);
}

fn connected_components_within(
    members: &[usize],
    neighbors: &[Vec<(usize, f64)>],
    comm: &[usize],
    community: usize,
) -> Vec<Vec<usize>> {
    let member_set: HashSet<usize> = members.iter().copied().collect();
    let mut visited = HashSet::new();
    let mut components = Vec::new();

    for &start in members {
        if visited.contains(&start) {
            continue;
        }
        let mut component = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(start);
        visited.insert(start);
        while let Some(node) = queue.pop_front() {
            component.push(node);
            for &(nbr, _) in &neighbors[node] {
                if member_set.contains(&nbr) && comm[nbr] == community && visited.insert(nbr) {
                    queue.push_back(nbr);
                }
            }
        }
        components.push(component);
    }
    components
}

// ---------------------------------------------------------------------------
// Persist communities to graph — source: stages/stage-3c.md §4
// ---------------------------------------------------------------------------

/// Remove Community and Process nodes (and their edges) left by a prior
/// clustering pass. Both node tables use `id` as primary key, so re-running
/// `cluster_graph` on an already-clustered graph would otherwise abort with
/// a duplicate-primary-key error instead of re-clustering (bench q12 scored
/// 0.000 because the harness clusters once at setup and once per label).
fn purge_prior_clustering(store: &GraphStore) -> Result<(), String> {
    for label in ["Community", "Process"] {
        store
            .execute_query(&format!("MATCH (n:{label}) DETACH DELETE n"))
            .map_err(|e| format!("purge {label}: {e}"))?;
    }
    Ok(())
}

fn persist_communities(
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

// ---------------------------------------------------------------------------
// Entry point: cluster_graph — source: stages/stage-3c.md §5
// ---------------------------------------------------------------------------

pub fn cluster_graph(store: &GraphStore, gamma: f64) -> Result<ClusteringResult, String> {
    let start = Instant::now();
    purge_prior_clustering(store)?;
    let adj = extract_adjacency(store)?;

    let (mut comm, modularity) = louvain(&adj, gamma);
    repair_c2(&adj, &mut comm);

    let communities = persist_communities(store, &adj, &comm, modularity, gamma)?;
    let processes = trace_processes(store)?;

    Ok(ClusteringResult {
        communities,
        modularity,
        processes,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "community_tests.rs"]
mod tests;
