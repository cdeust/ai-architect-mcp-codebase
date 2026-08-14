//! Corpus indexing + provenance hashing. The committed corpus IS the pin
//! (§AC5): its content hash is recorded in `MANIFEST.md`, so a third party can
//! confirm they reran against the exact bytes these numbers came from.

use crate::conditions::visit_files;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, IndexOptions};
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
    indexer::index_codebase_with_language(&src, &graph_path, &IndexOptions::default())?;
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
    hex_lower(&hasher.finalize())
}

/// Lowercase hex, two digits per byte, no separators or `0x` prefix.
///
/// Replaces `format!("{:x}", digest)`. sha2 0.11 returns `hybrid_array::Array`
/// instead of 0.10's `GenericArray`, and `Array` does not implement
/// `LowerHex`, so the old formatting no longer compiles.
///
/// The output is byte-for-byte what `{:x}` produced — `GenericArray`'s
/// `LowerHex` impl also emitted exactly two lowercase hex digits per byte with
/// no separator. That equality is load-bearing, not cosmetic:
/// `corpus_hash` is a REPRODUCIBILITY IDENTIFIER recorded alongside benchmark
/// results, so a changed representation would silently invalidate every
/// previously recorded run rather than fail loudly. Pinned by
/// `hex_lower_matches_the_previous_format` against a published SHA-256 vector.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the digest STRING FORMAT across the sha2 0.10 -> 0.11 move.
    ///
    /// `corpus_hash` is recorded next to benchmark results, so if `hex_lower`
    /// ever formatted differently from the `{:x}` it replaced (a missing
    /// zero-pad, uppercase, a separator), previously recorded corpus hashes
    /// would stop matching and the benchmarks would look like they ran on a
    /// different corpus. A published vector makes that falsifiable rather than
    /// assumed.
    ///
    /// source: NIST FIPS 180-4 / RFC 6234 SHA-256 test vector for "abc".
    #[test]
    fn hex_lower_matches_the_previous_format() {
        let mut hasher = Sha256::new();
        hasher.update(b"abc");
        assert_eq!(
            hex_lower(&hasher.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Zero-padding: a byte below 0x10 must still occupy two digits.
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(hex_lower(&[]), "");
    }

    fn corpus_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
    }

    /// `source_map` returns the language's real source files keyed by relative
    /// path, with their true contents. Kills the `source_map` return mutants
    /// (empty map + the `("xyzzy"…)` / `("")` dummy singletons): the real map
    /// contains `core.py` whose body defines `process_order`, which no dummy
    /// reproduces.
    #[test]
    fn source_map_carries_real_files_and_contents() {
        let map = source_map(&corpus_root(), "python");
        assert!(
            map.len() >= 2,
            "python corpus has multiple files, got {}",
            map.len()
        );
        let core = map
            .get("core.py")
            .expect("source_map must key files by their relative path (core.py)");
        assert!(
            core.contains("process_order"),
            "core.py content must be the real source (defines process_order)"
        );
        // No dummy key leaked in place of the real relative paths.
        assert!(
            !map.contains_key("xyzzy") && !map.contains_key(""),
            "source_map must not contain placeholder keys"
        );
    }
}
