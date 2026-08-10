// clustering::community_adjacency — builds the weighted symbol adjacency
// Louvain runs over: which nodes are clusterable, which edges connect them,
// and how much each edge kind counts. source: stages/stage-3c.md §2.4.
//
// Extracted from community.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// clustering vocabulary exactly as when this lived in one module.

use super::*;

/// Collected cluster-eligible node identity: (ids, labels, id-to-adjacency-
/// index map), all index-aligned. clippy::type_complexity — named so the
/// shape is documented once instead of repeated at every call site.
pub(super) type SymbolNodeCollection = (Vec<String>, Vec<String>, HashMap<String, usize>);

/// Per-node weighted neighbor lists (adjacency[i] = [(neighbor_idx, weight)])
/// plus the total edge weight summed across the whole adjacency.
/// clippy::type_complexity.
pub(super) type WeightedAdjacency = (Vec<Vec<(usize, f64)>>, f64);

pub(super) struct Adjacency {
    pub(super) node_ids: Vec<String>,
    pub(super) node_labels: Vec<String>,
    #[allow(dead_code)] // used by tests for constructing test adjacencies
    pub(super) id_to_idx: HashMap<String, usize>,
    pub(super) neighbors: Vec<Vec<(usize, f64)>>,
    pub(super) total_weight: f64,
}

/// Node labels fed to Louvain: the clusterable symbols plus File nodes.
/// File nodes act as containment carriers — Defines_File_* edges bind
/// same-file symbols, which is the dominant module-affinity signal (measured
/// on the rust-self bench graph: ~1129 Defines_File_* edges vs ~1559 Calls;
/// without them the adjacency has no file/module co-membership signal at all
/// and the partition cannot recover the directory-based ground truth).
/// Directory nodes stay excluded (stage-3c.md §2.4). File memberships are
/// NOT persisted — see `community_persist::persist_communities`.
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

pub(super) fn extract_adjacency(store: &GraphStore) -> Result<Adjacency, String> {
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

pub(super) fn collect_symbol_nodes(store: &GraphStore) -> Result<SymbolNodeCollection, String> {
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

pub(super) fn collect_weighted_edges(
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
