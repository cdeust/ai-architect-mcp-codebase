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

/// The walk and parse settings a graph was built with (issue #443).
///
/// The per-file states say WHICH files changed; they cannot say whether the
/// caller now asks for a different language filter, dependency tier or set of
/// excluded directories, any of which changes which files belong in the graph
/// at all. A pass that wants to reuse the graph incrementally compares this
/// value with its own settings and rebuilds in full on any difference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexScope {
    /// The version of the server that built the graph. A newer build may parse
    /// or resolve differently, so its graph is not reused across versions.
    #[serde(default)]
    pub indexer_version: Option<String>,
    /// `None` for per-file auto-detection, else the forced language.
    pub language: Option<String>,
    pub dependency_scope: String,
    /// `ExcludeSet::canonical_entries`: sorted, kind-tagged.
    pub exclude_dirs: Vec<String>,
}

/// The whole sidecar: schema tag + rel_path → state map. `BTreeMap` keeps the
/// serialized order stable so the JSON diffs cleanly across re-indexes.
///
/// `scope` is optional so a manifest written before issue #443 still loads; an
/// absent scope means "settings unknown", never "settings match".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileManifest {
    pub schema_version: u32,
    /// At most `MAX_FILES` entries when read back (`bounded_files`).
    #[serde(deserialize_with = "bounded_files")]
    pub files: BTreeMap<String, FileState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<IndexScope>,
    /// `CargoFacts::digest` of the pass that last wrote this manifest (review of
    /// #478), which a reuse requires to be the map the graph holds:
    /// the Cargo map the resolver reads comes from manifests the walk may not
    /// list (`Cargo.toml` under `language: "rust"`, a path dependency outside
    /// the tree), so the per-file states cannot see it change. Absent means
    /// "unknown", never "unchanged".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_facts: Option<String>,
}

impl FileManifest {
    /// A fresh, empty manifest carrying the current schema version.
    pub fn new() -> Self {
        FileManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            files: BTreeMap::new(),
            scope: None,
            cargo_facts: None,
        }
    }

    /// This manifest, recording `digest` as the Cargo map of the pass that
    /// built the graph. A full index takes its snapshot before it runs (so a
    /// file saved during it reads as changed) and only knows the map after.
    pub fn with_cargo_facts(mut self, digest: &str) -> Self {
        self.cargo_facts = Some(digest.to_string());
        self
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
/// sidecar degrades to a full rebuild instead of failing the tool. A file over
/// `MAX_MANIFEST_BYTES` is `None` too, and is read no further than that.
pub fn load(path: &Path) -> Option<FileManifest> {
    load_within(path, MAX_MANIFEST_BYTES)
}

/// A practical byte budget for a manifest: `MAX_FILES` entries averaging 1280
/// bytes (a root-relative path plus its pretty-printed `FileState`, about 200
/// bytes of which are fixed), about 128 MB. A real tree averages a fraction of
/// that. A larger file reads as absent, so the caller rebuilds; it is never
/// read whole (Codex security reviews of #478: a compressible archive entry
/// expands to gigabytes). The entry count is bounded separately while parsing,
/// because short entries under this budget would still number in millions.
pub const MAX_MANIFEST_BYTES: u64 = super::MAX_FILES as u64 * 1280;

/// Deserializes `files`, failing at the first entry past `MAX_FILES`, before
/// the map grows further: no index writes more, and a planted manifest of
/// millions of short entries would otherwise allocate far more than its bytes.
fn bounded_files<'de, D>(deserializer: D) -> Result<BTreeMap<String, FileState>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Bounded;
    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = BTreeMap<String, FileState>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a map of at most {} file states", super::MAX_FILES)
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut files = BTreeMap::new();
            while let Some((rel, state)) = map.next_entry::<String, FileState>()? {
                if files.len() >= super::MAX_FILES {
                    return Err(serde::de::Error::custom(format!(
                        "more than {} file states",
                        super::MAX_FILES
                    )));
                }
                files.insert(rel, state);
            }
            Ok(files)
        }
    }
    deserializer.deserialize_map(Bounded)
}

/// `load` under an explicit cap, so a test can reach the refusal.
fn load_within(path: &Path, cap: u64) -> Option<FileManifest> {
    parse(&read_regular_capped(path, cap)?.0)
}

/// The bytes of the sidecar at `path` and the metadata of the handle they came
/// from, or `None` when `path` is not a regular file or holds more than `cap`
/// bytes (reviews of #478). A sidecar can arrive from an artifact, so a FIFO
/// (whose `open` blocks), a link to `/dev/zero` (endless) or a compressible
/// entry that expands to gigabytes must not be read: the type is checked
/// before `open` and again on the handle, and the read stops past `cap`.
pub(crate) fn read_regular_capped(path: &Path, cap: u64) -> Option<(Vec<u8>, fs::Metadata)> {
    use std::io::Read;
    if !fs::symlink_metadata(path).ok()?.file_type().is_file() {
        eprintln!(
            "[ap] sidecar {} is not a regular file; treated as absent",
            path.display()
        );
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > cap {
        eprintln!(
            "[ap] sidecar {} is over {cap} bytes; treated as absent",
            path.display()
        );
        return None;
    }
    Some((bytes, meta))
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
    let (bytes, meta) = read_regular_capped(path, MAX_MANIFEST_BYTES)?;
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
    fs::read(path).ok().map(|bytes| hash_bytes(&bytes))
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
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
    fn a_manifest_with_more_entries_than_max_files_is_refused() {
        // Codex security review of #478: short entries under the byte budget
        // could number in millions; parsing stops at the first past MAX_FILES.
        let entry = r#"{"mtime_ns":0,"size":0,"content_hash":""}"#;
        let body = |n: usize| {
            let files: Vec<String> = (0..n).map(|i| format!("\"{i}\":{entry}")).collect();
            format!(r#"{{"schema_version":1,"files":{{{}}}}}"#, files.join(","))
        };
        let max = super::super::MAX_FILES;
        assert_eq!(
            parse(body(max).as_bytes())
                .expect("at the limit")
                .files
                .len(),
            max
        );
        assert!(
            parse(body(max + 1).as_bytes()).is_none(),
            "one past it is refused"
        );
    }

    #[test]
    fn a_manifest_over_the_cap_is_refused() {
        // Codex security review of #478: an oversized manifest is treated as
        // absent rather than read whole.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(MANIFEST_FILE);
        save(&path, &FileManifest::new()).expect("save");
        let size = fs::metadata(&path).expect("stat").len();
        assert!(
            load_within(&path, size).is_some(),
            "the cap itself is allowed"
        );
        assert!(
            load_within(&path, size - 1).is_none(),
            "one byte over is refused"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_manifest_that_is_not_a_regular_file_is_not_opened() {
        // Codex review of #478: a FIFO with no writer blocks `open` forever,
        // so reaching the assertions means nothing opened it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(MANIFEST_FILE);
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo");
        assert!(made.success());
        assert!(load(&path).is_none());
        assert!(load_with_identity(&path).is_none());
    }

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
