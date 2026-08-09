use super::*;

// ---------------------------------------------------------------------------
// Determinism regression (issue full-ast-index review): `collect_symbol_nodes`
// and `collect_weighted_edges` query the store with an unordered `MATCH`,
// which handed `one_level`'s greedy local-move phase a node-processing order
// (and a float-summation order inside `ki_in`) that depends on the store's
// internal scan order rather than the graph's CONTENT. On a small graph every
// reasonable scan order agreed by luck; on a larger one (this repo's own
// src/, ~500+ symbol nodes after the full-AST persistence PR) two
// back-to-back `cluster_graph` calls on the SAME unchanged graph produced
// DIFFERENT partitions (observed: a 3-community PRD-validation fixture
// consistently and reproducibly regressed to see only 2 within one process's
// results, while a separate process run recovered 3) — a real, silent
// correctness bug that predates the PR which exposed it. Fixed by adding
// `ORDER BY n.id` / `ORDER BY a.id, b.id` to both queries, restoring the
// determinism `one_level`'s own "Sorted candidate order" comment already
// promises for its OWN tie-breaking (the promise was only half kept: the
// candidates were sorted, but the order they were fed into `one_level` in
// the first place was not).
// ---------------------------------------------------------------------------

/// Asserts `collect_symbol_nodes` + `collect_weighted_edges` return
/// byte-identical results across two calls against the SAME store — the
/// precise mechanism `cluster_graph`'s reproducibility depends on. Uses a
/// tiny synthetic fixture: fast, and it does verify the mechanism, but a
/// small graph has too few near-tie modularity decisions to reliably
/// REPRODUCE the bug this guards (confirmed empirically — reverting the
/// `ORDER BY` fix and running this specific test 5x in a row still passed
/// every time on this fixture size). `cluster_graph_partition_is_stable_
/// on_this_repos_own_src` below is the test that actually catches it, at
/// the scale (~500+ symbol nodes) where the non-determinism was observed.
#[test]
fn symbol_and_edge_collection_is_deterministic_across_repeated_calls() {
    use crate::test_support::TempDirExt;
    let tmp = tempfile::Builder::new()
        .prefix("community_determinism_")
        .tempdir()
        .expect("tempdir")
        .keep_managed();
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Enough files/symbols to exercise more than one MATCH-per-label row.
    for i in 0..8 {
        std::fs::write(
            src.join(format!("m{i}.rs")),
            format!(
                "pub fn f{i}_a() {{ f{i}_b(); }}\n\
                 pub fn f{i}_b() {{}}\n\
                 pub struct S{i} {{ pub x: i32 }}\n"
            ),
        )
        .unwrap();
    }
    let graph_dir = tmp.join("graph");
    crate::indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    crate::resolver::resolve_graph(&store).expect("resolve");

    let (ids_a, labels_a, _) = collect_symbol_nodes(&store).expect("collect 1");
    let (ids_b, labels_b, _) = collect_symbol_nodes(&store).expect("collect 2");
    assert_eq!(
        ids_a, ids_b,
        "collect_symbol_nodes must return the same id ORDER across calls \
         (feeds one_level's node-processing index directly)"
    );
    assert_eq!(labels_a, labels_b);

    let id_to_idx: HashMap<String, usize> = ids_a
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i))
        .collect();
    let n = ids_a.len();
    let (neighbors_a, weight_a) = collect_weighted_edges(&store, &id_to_idx, n).expect("edges 1");
    let (neighbors_b, weight_b) = collect_weighted_edges(&store, &id_to_idx, n).expect("edges 2");
    assert_eq!(
        neighbors_a, neighbors_b,
        "collect_weighted_edges must return the same per-node adjacency \
         ORDER across calls (feeds ki_in's float-summation order directly)"
    );
    assert_eq!(weight_a, weight_b);
}

/// End-to-end, at the scale the bug actually manifested at, and reproducing
/// the ACTUAL shape of the observed regression: TWO SEPARATE fresh graphs
/// (two independent `index_codebase` + `resolve_graph` + `cluster_graph`
/// runs into two different directories), not two `cluster_graph` calls
/// reusing one already-built store.
///
/// That distinction is load-bearing, found empirically: calling
/// `cluster_graph` twice against the SAME already-built store did NOT
/// reproduce the bug (10+ repeated attempts, pre-fix code, all passed) — the
/// unordered `MATCH` inside an unchanging, already-materialized database
/// apparently scans in a stable order once written. The REAL non-determinism
/// (confirmed via `cargo run --release -p bench-end-result`, four consecutive
/// separate process invocations, all landing on the same WRONG 2-community
/// partition against a baseline that was reliably 3) is across independently
/// BUILT databases from identical source: lbug's bulk-insert path has no
/// documented row-order guarantee, so two fresh builds of the identical
/// graph can lay out rows differently, and an unordered scan then exposes
/// that build-time non-determinism to Louvain's node-processing order.
/// Building two databases here reproduces that shape faithfully.
///
/// Uses THIS repo's own `src/` (the same fixture `indexer::tests::test_
/// index_own_project` uses, ~500+ symbol nodes) — a small synthetic fixture
/// has too few near-tie modularity decisions to reproduce this (see the
/// note on `symbol_and_edge_collection_is_deterministic_across_repeated_
/// calls` above).
#[test]
fn cluster_graph_partition_is_stable_on_this_repos_own_src() {
    use crate::test_support::TempDirExt;
    let tmp = tempfile::Builder::new()
        .prefix("community_determinism_e2e_")
        .tempdir()
        .expect("tempdir")
        .keep_managed();

    let build_and_cluster = |name: &str| -> std::collections::BTreeMap<String, String> {
        let graph_dir = tmp.join(name);
        crate::indexer::index_codebase(std::path::Path::new("src"), &graph_dir).expect("index");
        let store = GraphStore::open_or_create(&graph_dir).unwrap();
        crate::resolver::resolve_graph(&store).expect("resolve");
        cluster_graph(&store, 1.0).expect("cluster");
        let cypher = "MATCH (n:Function)-[:MemberOf_Function_Community]->(c:Community) \
                       RETURN n.qualified_name, c.id ORDER BY n.qualified_name";
        let qr = store.execute_query(cypher).unwrap();
        qr.rows
            .iter()
            .map(|r| (r[0].clone(), r[1].clone()))
            .collect()
    };

    let comm_a = build_and_cluster("graph_a");
    let comm_b = build_and_cluster("graph_b");

    assert_eq!(
        comm_a.keys().collect::<Vec<_>>(),
        comm_b.keys().collect::<Vec<_>>(),
        "the set of clustered Function symbols changed across two \
         independent index+resolve+cluster runs of the identical source"
    );
    // Compare partitions up to relabeling: two symbols in the same community
    // on run A must be in the same community on run B, and vice versa. The
    // three q13 fixture symbols specifically, since that is the exact
    // regression this test exists to catch.
    let names: Vec<&String> = comm_a.keys().collect();
    for i in 0..names.len() {
        for j in (i + 1)..names.len() {
            let same_a = comm_a[names[i]] == comm_a[names[j]];
            let same_b = comm_b[names[i]] == comm_b[names[j]];
            assert_eq!(
                same_a, same_b,
                "partition changed across repeated cluster_graph calls on an \
                 unchanged graph: ({}, {}) co-membership flipped",
                names[i], names[j]
            );
        }
    }
    for qn in [
        "parser/mod.rs::parse_file",
        "search/mod.rs::search_graph",
        "clustering/community.rs::cluster_graph",
    ] {
        assert!(
            comm_a.contains_key(qn),
            "expected {qn} to be a clustered Function symbol in this repo's own graph"
        );
    }
    let ids: std::collections::BTreeSet<&String> = [
        &comm_a["parser/mod.rs::parse_file"],
        &comm_a["search/mod.rs::search_graph"],
        &comm_a["clustering/community.rs::cluster_graph"],
    ]
    .into_iter()
    .collect();
    assert_eq!(
        ids.len(),
        3,
        "parse_file/search_graph/cluster_graph must land in 3 distinct \
         communities (benches/corpora/rust-self's q13 PRD-validation \
         fixture depends on this): got {ids:?}"
    );
}

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
