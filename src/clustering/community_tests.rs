use super::*;

#[test]
fn test_louvain_two_cliques() {
    // Two 3-node cliques connected by one bridge edge
    let node_ids: Vec<String> = (0..6).map(|i| format!("n{i}")).collect();
    let node_labels: Vec<String> = vec!["Function".into(); 6];
    let id_to_idx: HashMap<String, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i))
        .collect();

    let mut neighbors: Vec<Vec<(usize, f64)>> = vec![Vec::new(); 6];
    let edges = [(0, 1), (1, 2), (0, 2), (3, 4), (4, 5), (3, 5), (2, 3)];
    let mut total_weight = 0.0;
    for &(a, b) in &edges {
        neighbors[a].push((b, 3.0));
        neighbors[b].push((a, 3.0));
        total_weight += 3.0;
    }

    let adj = Adjacency {
        node_ids,
        node_labels,
        id_to_idx,
        neighbors,
        total_weight,
    };
    let (mut comm, q) = louvain(&adj, 1.0);
    repair_c2(&adj, &mut comm);

    // Should find 2 communities
    let unique: HashSet<usize> = comm.iter().copied().collect();
    assert!(
        unique.len() == 2,
        "expected 2 communities, got {} (comm: {:?})",
        unique.len(),
        comm
    );
    // Nodes 0,1,2 in same community
    assert_eq!(comm[0], comm[1]);
    assert_eq!(comm[1], comm[2]);
    // Nodes 3,4,5 in same community
    assert_eq!(comm[3], comm[4]);
    assert_eq!(comm[4], comm[5]);
    // Different communities
    assert_ne!(comm[0], comm[3]);
    assert!(q > 0.0, "modularity should be positive, got {q}");
}

#[test]
fn test_louvain_phase2_merges_ring_of_triangles() {
    // Ring of q=12 triangles (unit weights), each linked to the next by one
    // bridge edge. Resolution-limit arithmetic (Fortunato & Barthélemy 2007,
    // PNAS 104(1)): Q(one triangle per community) = 3/4 - 1/q, Q(adjacent
    // pairs) = 7/8 - 2/q, so pairing wins for q > 8. Single-node moves alone
    // cannot merge two triangles (each is a local optimum); only phase 2
    // aggregation (Blondel 2008 §II) can, so this test fails on a
    // phase-1-only implementation with exactly 12 communities.
    let q = 12;
    let n = 3 * q;
    let node_ids: Vec<String> = (0..n).map(|i| format!("n{i}")).collect();
    let node_labels: Vec<String> = vec!["Function".into(); n];
    let id_to_idx: HashMap<String, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i))
        .collect();

    let mut neighbors: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut total_weight = 0.0;
    let add_edge = |nbrs: &mut Vec<Vec<(usize, f64)>>, a: usize, b: usize| {
        nbrs[a].push((b, 1.0));
        nbrs[b].push((a, 1.0));
    };
    for t in 0..q {
        let base = 3 * t;
        add_edge(&mut neighbors, base, base + 1);
        add_edge(&mut neighbors, base + 1, base + 2);
        add_edge(&mut neighbors, base, base + 2);
        // bridge from this triangle to the next one in the ring
        add_edge(&mut neighbors, base + 2, (base + 3) % n);
        total_weight += 4.0;
    }

    let adj = Adjacency {
        node_ids,
        node_labels,
        id_to_idx,
        neighbors,
        total_weight,
    };
    let (comm, q_mod) = louvain(&adj, 1.0);

    let unique: HashSet<usize> = comm.iter().copied().collect();
    assert!(
        unique.len() < q && unique.len() > 1,
        "phase 2 must merge triangles: expected 1 < communities < {q}, got {}",
        unique.len()
    );
    // No triangle may be split across communities.
    for t in 0..q {
        let base = 3 * t;
        assert_eq!(comm[base], comm[base + 1], "triangle {t} split");
        assert_eq!(comm[base + 1], comm[base + 2], "triangle {t} split");
    }
    // Pairing beats one-triangle-per-community: Q must exceed 3/4 - 1/q.
    assert!(
        q_mod > 0.75 - 1.0 / (q as f64),
        "modularity {q_mod} not above the singles partition bound"
    );
}

#[test]
fn test_renumber_communities() {
    let comm = vec![5, 5, 3, 3, 5, 10];
    let renumbered = renumber_communities(&comm);
    assert_eq!(renumbered[0], renumbered[1]);
    assert_eq!(renumbered[2], renumbered[3]);
    assert_eq!(renumbered[0], renumbered[4]);
    let unique: HashSet<usize> = renumbered.iter().copied().collect();
    assert_eq!(unique.len(), 3);
}
