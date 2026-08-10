// clustering::community_louvain — Blondel et al. 2008 modularity
// optimization. source: "Fast unfolding of communities in large networks".
//
// Extracted from community.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// clustering vocabulary exactly as when this lived in one module.

use super::*;

/// Blondel et al. 2008, section II: "the two phases are repeated
/// iteratively". Phase 1 (`one_level`) moves individual nodes; phase 2
/// (`aggregate_graph`) condenses each community into a super-node so whole
/// communities can merge on the next pass. Iterating until a pass produces
/// no merge is what lets the partition reach coarse module granularity —
/// phase 1 alone leaves the graph over-fragmented (measured on the rust-self
/// bench corpus: 1262 communities for 2567 symbols without phase 2).
pub(super) fn louvain(adj: &Adjacency, gamma: f64) -> (Vec<usize>, f64) {
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
pub(super) fn one_level(neighbors: &[Vec<(usize, f64)>], m: f64, gamma: f64) -> Vec<usize> {
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

pub(super) fn renumber_communities(comm: &[usize]) -> Vec<usize> {
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
