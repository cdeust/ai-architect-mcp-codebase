//! Corpus indexing + provenance hashing. The committed corpus IS the pin
//! (§AC5): its content hash is recorded in `MANIFEST.md`, so a third party can
//! confirm they reran against the exact bytes these numbers came from.

use crate::conditions::visit_files;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, DependencyScope};
use ai_architect_mcp::{clustering, resolver};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A language corpus indexed into a throwaway graph, ready to query. The
/// [`TempDir`] is held so the on-disk graph outlives the queries.
pub struct Indexed {
    pub store: GraphStore,
    pub graph_path: PathBuf,
    _tmp: TempDir,
}

/// Index one language's corpus through the real pipeline
/// (index → resolve → cluster), the exact steps the MCP tools run.
pub fn index_language(corpus_root: &Path, lang: &str) -> Result<Indexed, String> {
    let src = corpus_root.join(lang);
    let tmp = tempfile::Builder::new()
        .prefix("eval_h2h_")
        .tempdir()
        .map_err(|e| format!("tempdir: {e}"))?;
    let graph_path = tmp.path().join("graph");
    indexer::index_codebase_with_language(&src, &graph_path, None, DependencyScope::None)?;
    let store = GraphStore::open_or_create(&graph_path)?;
    resolver::resolve_graph(&store)?;
    clustering::cluster_graph(&store, 1.0)?;
    Ok(Indexed {
        store,
        graph_path,
        _tmp: tmp,
    })
}

/// Read one language's source files into a `relpath → contents` map (fed to the
/// blinded judge so it grades against actual source).
pub fn source_map(corpus_root: &Path, lang: &str) -> BTreeMap<String, String> {
    let src = corpus_root.join(lang);
    let mut map = BTreeMap::new();
    visit_files(&src, &mut |rel, content| {
        map.insert(rel.to_string(), content.to_string());
    });
    map
}

/// SHA-256 over the whole corpus: for each file (sorted by relative path) the
/// path, a NUL, the byte length, a NUL, and the bytes. Deterministic and
/// order-independent of the filesystem walk.
pub fn corpus_hash(corpus_root: &Path) -> String {
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    visit_files(corpus_root, &mut |rel, content| {
        files.insert(rel.to_string(), content.as_bytes().to_vec());
    });
    let mut hasher = Sha256::new();
    for (rel, bytes) in &files {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(bytes.len().to_le_bytes());
        hasher.update([0u8]);
        hasher.update(bytes);
    }
    format!("{:x}", hasher.finalize())
}
