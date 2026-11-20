//! The analysis seal (issue #443): a sidecar beside a graph that says its
//! derived phases (resolve, cluster, search index) are current for the graph
//! as it stands.
//!
//! # Why it lives in the store
//!
//! `analyze_codebase` may skip those phases only when no other tool wrote to
//! the graph since the analysis that wrote the seal. Every writer opens the
//! graph through `GraphStore::open_or_create`, which withdraws the seal before
//! it opens (`withdraw`), so the seal cannot outlive a write it did not see,
//! whichever tool made it. The read cache (`open_for_cache`) leaves it alone.
//! A fingerprint of the graph's files would not do: the index pass writes the
//! Cargo facts again on every pass, even when nothing changed.
//!
//! The store owns only where the seal lives and its withdrawal; what it
//! records, and when it is written, are the composition root's
//! (`analyze_handlers::seal`), which writes it once every handle it opened on
//! the graph is closed.

use std::path::{Path, PathBuf};

/// `<graph_dir>.analysis_seal.json`, beside the graph directory: removing or
/// replacing the graph never leaves it inside.
pub fn seal_path(graph_dir: &Path) -> PathBuf {
    let mut name = graph_dir.file_name().unwrap_or_default().to_os_string();
    name.push(".analysis_seal.json");
    graph_dir.with_file_name(name)
}

/// Removes the seal of `graph_dir`. Absent is success; any other failure is an
/// error, because a seal that stays would vouch for a write it did not see.
pub fn withdraw(graph_dir: &Path) -> Result<(), String> {
    let path = seal_path(graph_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "cannot withdraw the analysis seal {}: {e}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_store::GraphStore;

    #[test]
    fn the_seal_sits_beside_the_graph() {
        assert_eq!(
            seal_path(Path::new("/out/graph")),
            PathBuf::from("/out/graph.analysis_seal.json")
        );
    }

    #[test]
    fn opening_the_graph_for_writing_withdraws_the_seal() {
        let tmp = tempfile::tempdir().expect("tmp");
        let graph = tmp.path().join("graph");
        std::fs::write(seal_path(&graph), b"{}").expect("seal");
        let store = GraphStore::open_or_create(&graph).expect("open");
        assert!(
            !seal_path(&graph).exists(),
            "an open for writing withdraws it"
        );
        drop(store);
        std::fs::write(seal_path(&graph), b"{}").expect("seal again");
        let cached = GraphStore::open_for_cache(&graph).expect("open for the cache");
        assert!(seal_path(&graph).exists(), "the read cache leaves it");
        drop(cached);
    }

    #[test]
    fn withdrawing_an_absent_seal_succeeds() {
        let tmp = tempfile::tempdir().expect("tmp");
        withdraw(&tmp.path().join("graph")).expect("absent is fine");
    }
}
