// clustering::community_repair — C2 repair: split disconnected communities.
// source: Traag 2019 §3.2.
//
// Extracted from community.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// clustering vocabulary exactly as when this lived in one module.

use super::*;

pub(super) fn repair_c2(adj: &Adjacency, comm: &mut Vec<usize>) {
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
