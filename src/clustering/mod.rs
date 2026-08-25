// clustering — Stage 3c: community detection + process tracing.
//
// Clustering: Louvain algorithm (Blondel et al. 2008, J Stat Mech P10008)
// implemented in Rust inline (Branch C — lbug 0.15.3 algo extension
// requires network download, unavailable in-process).
// Post-processing: C2 repair pass splits disconnected communities
// (Traag et al. 2019, Scientific Reports 9(1), 5233, section 3.2).
//
// Process tracing: BFS from entry points along Calls edges.
// source: stages/stage-3c.md §2, §3

mod community;
mod impact;
mod process;

// ---------------------------------------------------------------------------
// Symbol labels eligible for clustering — source: stage-3c.md §2.4
// ---------------------------------------------------------------------------

pub(crate) const SYMBOL_LABELS: &[&str] = &[
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Trait",
    "Constant",
    "TypeAlias",
    "Module",
];

// The binary crate (src/main.rs `mod clustering`) consumes only part of this
// public surface; the library crate (src/lib.rs `pub mod clustering`) and the
// integration tests use the rest. Re-exporting the full original surface keeps
// every `clustering::X` path valid; `allow(unused_imports)` suppresses the
// per-binary "unused re-export" warning that fires when a given binary does not
// touch every item.
#[allow(unused_imports)]
pub use community::{
    cluster_graph, cluster_id_from_community_id, collect_cluster_memberships, ClusterMembership,
    ClusterMemberships, ClusteringResult,
};
#[allow(unused_imports)]
pub use impact::{get_impact, ImpactNode, ImpactResult};
#[allow(unused_imports)]
pub use process::{get_processes, trace_processes, ProcessInfo};
