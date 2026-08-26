// manifest — per-file state sidecar for incremental indexing (issue #62).
//
// Layer: shared/persistence within the indexer. This module owns the on-disk
// record of "what the graph was built from": for every indexed file a
// (mtime_ns, size, content_hash) triple, keyed by the same repo-relative,
// forward-slash id the File node carries in the graph. It performs filesystem
// I/O (read/stat/hash a file, read/write the JSON sidecar) and nothing graph-
// related — it never touches `GraphStore` or `lbug`. The incremental policy
// (`super::incremental`) consumes these states to classify changes.
//
// Why a sidecar and not the graph: the reference (DeusData/codebase-memory-mcp
// `src/pipeline/pipeline_incremental.c`) stores per-file (mtime_ns, size) hashes
// in its SQLite store and classifies changed files by comparing stat() against
// them. AP keeps the graph portable (relative paths only, no machine-specific
// mtimes baked in), so the mutable per-machine state lives beside the graph in
// `<output_dir>/file_manifest.json`, regenerated on every index — exactly like
// the existing `meta.json` root sidecar. AP's documented improvement over the C
// reference: we also store a content hash per file, which powers both a
// correctness fallback (mtime changed but bytes identical → not a real change)
// and rename detection (a delete+add pair with an identical hash is a rename;
// see `super::incremental`).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Sidecar schema version. Bump when the layout changes incompatibly; a newer
/// schema is treated as "no usable manifest" (→ full index) rather than
/// mis-parsed, so an old binary never classifies against a format it cannot read.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// The manifest filename, written beside the graph in the tool's `output_dir`.
pub const MANIFEST_FILE: &str = "file_manifest.json";

/// Persisted per-file state: the exact triple change-classification compares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    /// File modification time in nanoseconds since the Unix epoch. The cheap
    /// hot-path change signal (mtime OR size differs → candidate change).
    pub mtime_ns: i64,
    /// File size in bytes. Second half of the cheap change signal.
    pub size: u64,
    /// Lowercase hex SHA-256 of the file's bytes. The correctness fallback
    /// (mtime moved but bytes are identical → unchanged) and the rename key.
    pub content_hash: String,
}

/// The whole sidecar: schema tag + rel_path → state map. `BTreeMap` keeps the
/// serialized order stable so the JSON diffs cleanly across re-indexes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileManifest {
    pub schema_version: u32,
    pub files: BTreeMap<String, FileState>,
}

impl FileManifest {
    /// A fresh, empty manifest carrying the current schema version.
    pub fn new() -> Self {
        FileManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            files: BTreeMap::new(),
        }
    }
}

/// The manifest path for a given tool `output_dir` (sibling of `graph/`).
pub fn manifest_path(output_dir: &Path) -> PathBuf {
    output_dir.join(MANIFEST_FILE)
}

/// Loads the manifest at `path`.
///
/// Preconditions: none. Postconditions: returns `Some(manifest)` only when the
/// file exists, parses, and carries a schema this build understands; returns
/// `None` for every other case (absent, unreadable, unparseable, or a newer
/// schema). `None` is the caller's signal that no incremental baseline exists,
/// so it must fall back to a full index — never a hard error, so a corrupt
/// sidecar degrades to a full rebuild instead of failing the tool.
pub fn load(path: &Path) -> Option<FileManifest> {
    let bytes = fs::read(path).ok()?;
    parse(&bytes)
}

/// Loads the manifest at `path` together with the identity of the exact file
/// the bytes came from, as `(manifest, size, mtime_ns)`.
///
/// One `open` then `fstat` on THAT handle, not a read followed by a separate
/// `stat` of the same name: sampling the path twice lets a rewrite land between
/// them and pairs one index's bytes with another index's identity. The caller —
/// `graph_freshness` — compares this identity against what `meta.json` claims
/// to accompany, so a mismatch there must mean "the pair is wrong", never "the
/// pair was re-read at the wrong moment" (fleet-watch#112 review round 6).
pub fn load_with_identity(path: &Path) -> Option<(FileManifest, u64, i64)> {
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut bytes).ok()?;
    Some((parse(&bytes)?, meta.len(), mtime_ns(&meta)))
}

/// Decodes manifest bytes, rejecting a schema this build does not understand.
fn parse(bytes: &[u8]) -> Option<FileManifest> {
    let manifest: FileManifest = serde_json::from_slice(bytes).ok()?;
    if manifest.schema_version > MANIFEST_SCHEMA_VERSION {
        return None;
    }
    Some(manifest)
}

/// Writes `manifest` to `path` atomically, through the shared
/// `handler_util::write_json_atomic`.
///
/// Preconditions: none. Postconditions on `Ok`: `path` contains the serialized
/// manifest and no partial temp file remains; on `Err`, `path` is left
/// untouched (the temp write or the rename failed before replacing it).
///
/// The shared helper rather than a local temp+rename (fleet-watch#112 review
/// round 6): this used to hand-roll one with a FIXED `file_manifest.json.tmp`
/// name and no `fsync`, so two concurrent indexers of one `output_dir` could
/// interleave through the shared temp path, and a crash between the write and
/// the rename could publish a truncated manifest. `meta.json` — written into
/// the same directory, and paired against this file by `graph_freshness` — was
/// given the shared helper in round 5; leaving this half hand-rolled undercut
/// the pairing defence that reads both.
pub fn save(path: &Path, manifest: &FileManifest) -> Result<(), String> {
    crate::atomic_file::write_json_atomic(path, manifest)
        .map(|_| ())
        .map_err(|e| format!("manifest: {e}"))
}

/// File modification time in nanoseconds since the Unix epoch, or 0 when the
/// platform/filesystem does not report one. 0 is a safe sentinel: a file whose
/// mtime reads as 0 will simply fall through to the size/hash comparison, never
/// a false "unchanged".
pub fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Lowercase hex SHA-256 of the file at `path`, or `None` if it cannot be read.
///
/// source: the RustCrypto `sha2` crate already vendored for stage-2
/// transcript digests (Cargo.toml) — same primitive, reused; no new dependency.
pub fn hash_file(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(hex_lower(&hasher.finalize()))
}

/// Encodes bytes as a lowercase hex string.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // {:02x} is a fixed two-hex-digit format, not a "clever" one-liner:
        // one effect (append two chars), no coercion. source: std fmt.
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::Builder::new()
            .prefix("manifest_roundtrip_")
            .tempdir()
            .expect("temp dir");
        let path = manifest_path(dir.path());

        let mut m = FileManifest::new();
        m.files.insert(
            "src/a.rs".to_string(),
            FileState {
                mtime_ns: 123,
                size: 45,
                content_hash: "deadbeef".to_string(),
            },
        );
        save(&path, &m).expect("save");

        let loaded = load(&path).expect("load");
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files["src/a.rs"].size, 45);
        assert_eq!(loaded.files["src/a.rs"].content_hash, "deadbeef");
    }

    #[test]
    fn absent_or_newer_schema_yields_none() {
        let dir = tempfile::Builder::new()
            .prefix("manifest_none_")
            .tempdir()
            .expect("temp dir");
        let path = manifest_path(dir.path());
        // Absent → None.
        assert!(load(&path).is_none());
        // Newer schema → None (this build must not classify against it).
        let future = format!(
            "{{\"schema_version\": {}, \"files\": {{}}}}",
            MANIFEST_SCHEMA_VERSION + 1
        );
        fs::write(&path, future).unwrap();
        assert!(load(&path).is_none());
    }

    #[test]
    fn hash_is_stable_and_content_sensitive() {
        let dir = tempfile::Builder::new()
            .prefix("manifest_hash_")
            .tempdir()
            .expect("temp dir");
        let a = dir.path().join("a.txt");
        fs::write(&a, "hello").unwrap();
        let h1 = hash_file(&a).unwrap();
        let h2 = hash_file(&a).unwrap();
        assert_eq!(h1, h2, "same bytes → same hash");
        fs::write(&a, "hello!").unwrap();
        let h3 = hash_file(&a).unwrap();
        assert_ne!(h1, h3, "different bytes → different hash");
        // Known SHA-256 of "hello".
        assert_eq!(
            h1,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
    #[test]
    fn save_replaces_the_manifest_atomically_and_leaves_no_residue() {
        // Guards the reader-atomicity property of `save` (fleet-watch#112 review
        // round 6, finding 3): a reader opens the file, a second save lands, and
        // the held handle must still yield the whole previous version. No
        // wall-clock in the verdict.
        //
        // Scope, stated plainly: the hand-rolled write this replaced ALSO did
        // temp+rename, so it passed this test too. What routing through
        // `atomic_file` actually adds is the `fsync` before the rename and a
        // per-writer temp name — durability across a crash and safety against a
        // concurrent writer, neither of which a single-threaded test can
        // observe. Both were verified by inspection: the replaced body had no
        // `sync_all` and a fixed `file_manifest.json.tmp`. This test exists so
        // the property that IS observable cannot regress underneath them.
        use std::io::Read;
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("file_manifest.json");

        let mut first = FileManifest::new();
        first.files.insert(
            "only-in-the-first.rs".to_string(),
            FileState {
                mtime_ns: 1,
                size: 2,
                content_hash: String::new(),
            },
        );
        save(&path, &first).expect("first save");
        let mut held = fs::File::open(&path).expect("open manifest");

        save(&path, &FileManifest::new()).expect("second save");

        let mut seen = String::new();
        held.read_to_string(&mut seen).expect("read held handle");
        let parsed: FileManifest =
            serde_json::from_str(&seen).expect("an open reader must never see a torn manifest");
        assert!(
            parsed.files.contains_key("only-in-the-first.rs"),
            "the rewrite must land on a new inode, leaving the open reader's view whole",
        );
        assert!(
            load(&path).expect("reload").files.is_empty(),
            "the second save must win"
        );
        let residue: Vec<_> = fs::read_dir(tmp.path())
            .expect("read dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "file_manifest.json")
            .collect();
        assert!(
            residue.is_empty(),
            "no temp artifact may survive: {residue:?}"
        );
    }
}
