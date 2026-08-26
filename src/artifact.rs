// artifact — team-shared graph snapshot (export / bootstrap-import).
//
// Issue #55. Highest-leverage onboarding feature borrowed from
// DeusData/codebase-memory-mcp (`src/pipeline/artifact.c`, verified): a
// compressed graph snapshot committed to the indexed repository so a teammate
// who clones never has to cold-index.
//
// Layer: infrastructure/adapter. This module performs I/O only — filesystem,
// `git` shell-out, and (de)compression. It archives the on-disk graph *path*
// and never touches `lbug`, `GraphStore`, or any inner-layer type; the node /
// edge counts it records in the sidecar are passed in by the composition root
// (the `index_codebase` handler). Dependencies point inward: this file depends
// on std + `tar` + `zstd` + `serde` only, never the reverse (coding-standards
// §2.2 dependency rule, §5.1 core-declares-what-it-needs).
//
// Shape (mirrors artifact.c):
//   export = archive the graph dir (tar) → zstd-compress → write
//            `<repo>/.ai-architect-mcp-codebase/graph.zst` + a JSON sidecar
//            (schema version, git sha, tool version, node/edge counts); then
//            ensure a `.gitattributes` `merge=ours` entry so the binary blob
//            never produces a merge conflict across branches.
//   import = decompress → untar into `<output_dir>/graph`. The caller then
//            proceeds without a full cold index.
//
// AP-specific note vs. artifact.c: the C reference runs `VACUUM INTO` before
// snapshotting because its SQLite store is *live* (WAL frames could be torn,
// their #895). AP's store is CLOSED before export — `index_codebase` drops the
// `GraphStore` (flushing lbug) before this module runs — so a plain directory
// snapshot is already a consistent copy and no vacuum/checkpoint step exists.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::git_provenance::{commits_between, git_head, is_hex_sha};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Directory, relative to the indexed repo root, that holds the committed
/// artifact and its sidecar.
pub const ARTIFACT_DIR: &str = ".ai-architect-mcp-codebase";
/// The compressed graph snapshot filename.
pub const ARTIFACT_FILE: &str = "graph.zst";
/// The JSON metadata sidecar filename.
pub const ARTIFACT_META: &str = "graph.meta.json";

/// On-disk directory name before the project rename (issue #195). Recognized
/// ONLY by `migrate_legacy_dir` for a one-shot rename-in-place; every actual
/// artifact read/write goes through `ARTIFACT_DIR`. Not a permanent dual-path
/// read — project preference is explicit contracts + one-shot migrations,
/// never retro-compat shims.
pub(crate) const LEGACY_ARTIFACT_DIR: &str = ".automatised-pipeline";

/// Archive entry name for the bundled per-file manifest (issue #62). Kept equal
/// to `indexer::manifest::MANIFEST_FILE` so that unpacking the archive into the
/// output dir drops it exactly where the incremental classifier expects it; the
/// two are asserted equal in a test rather than coupled at compile time (artifact
/// is infrastructure, the manifest is an indexer leaf — no cross-import).
const ARTIFACT_MANIFEST_ENTRY: &str = "file_manifest.json";

/// Archive entry name for the bundled coverage report (issue #57). Kept equal to
/// `indexer::coverage::COVERAGE_FILE` (asserted in a test) so it unpacks beside
/// the graph where `index_status` / `query_graph(graph="missed")` look for it.
const ARTIFACT_COVERAGE_ENTRY: &str = "index_coverage.json";

/// Sidecar schema version. Bump when the sidecar or archive layout changes in
/// a way an older importer cannot read; import refuses a newer schema.
const SCHEMA_VERSION: u32 = 1;

/// zstd level for the (single) explicit-index tier.
///
/// Only one tier exists: the explicit `index_codebase` call is the sole export
/// caller, and it wants the best ratio. The reference (artifact.c) also carries
/// a fast zstd-3 tier for its *file watcher*; AP has no watcher, so shipping
/// that tier now would be a caller-less code path (coding-standards §9 — "if
/// it's built, it must be called"). The fast tier lands together with a watcher,
/// when one exists.
// source: DeusData/codebase-memory-mcp src/pipeline/artifact.c — ART_ZSTD_BEST = 9.
const ZSTD_LEVEL: i32 = 9;

/// Hard ceiling on the decompressed archive size. A crafted `.zst` that
/// declares a larger stream is rejected mid-decode before it can exhaust disk,
/// so a malicious committed artifact cannot turn a bootstrap into a DoS.
// source: DeusData/codebase-memory-mcp src/pipeline/artifact.c —
// ART_MAX_DECOMPRESSED_BYTES = 64 GiB (their note: a full Linux-kernel index is
// ~14 GB and fits comfortably under this ceiling).
const MAX_DECOMPRESSED_BYTES: u64 = 64 * 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The sidecar metadata committed alongside the compressed graph.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArtifactMeta {
    pub schema_version: u32,
    pub tool_version: String,
    /// git HEAD sha the artifact was exported at, or empty if the repo is not
    /// a git working tree.
    pub commit: String,
    pub node_count: u64,
    pub edge_count: u64,
    pub original_bytes: u64,
    pub compressed_bytes: u64,
    pub compression_level: i32,
}

/// Result of a successful export.
#[derive(Debug, Clone)]
pub struct ExportStats {
    pub artifact_path: PathBuf,
    pub original_bytes: u64,
    pub compressed_bytes: u64,
}

/// Staleness of a committed artifact relative to the repo's current git HEAD.
/// Only produced when the artifact is NOT fresh (its sha differs from HEAD, or
/// it carries no provenance sha at all).
#[derive(Debug, Clone, serde::Serialize)]
pub struct StaleInfo {
    /// git sha the artifact was exported at (empty string if the artifact
    /// carried no provenance — e.g. exported outside a git working tree).
    pub artifact_sha: String,
    /// repo's current git HEAD sha.
    pub head_sha: String,
    /// number of commits HEAD is ahead of the artifact, via
    /// `git rev-list --count <artifact_sha>..HEAD`. `None` when the artifact
    /// sha is unknown to this repo (or absent), so the count is unknowable.
    pub commits_behind: Option<u64>,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn artifact_dir(repo_path: &Path) -> PathBuf {
    repo_path.join(ARTIFACT_DIR)
}
fn artifact_file(repo_path: &Path) -> PathBuf {
    artifact_dir(repo_path).join(ARTIFACT_FILE)
}
fn meta_file(repo_path: &Path) -> PathBuf {
    artifact_dir(repo_path).join(ARTIFACT_META)
}

// ---------------------------------------------------------------------------
// Legacy-name migration (issue #195)
// ---------------------------------------------------------------------------

/// One-shot migration for a repo indexed before the project rename (#195):
/// renames `<repo>/.automatised-pipeline` to `<repo>/.ai-architect-mcp-codebase`
/// in place. No-op when the current dir already exists (it always wins — no
/// clobber) or the legacy dir is absent. Best-effort: a failed rename is
/// logged, never fatal, so a permissions issue degrades to "re-index instead
/// of bootstrap" rather than blocking the caller.
///
/// Called from every real touchpoint (`artifact_exists`, `export_artifact`,
/// `hook_augment::graph_present_fs`, and `index_codebase` before it walks the
/// tree) so the directory self-heals on first contact regardless of which
/// operation runs first.
pub fn migrate_legacy_dir(repo_path: &Path) {
    let legacy = repo_path.join(LEGACY_ARTIFACT_DIR);
    let current = artifact_dir(repo_path);
    if current.exists() || !legacy.exists() {
        return;
    }
    if let Err(e) = fs::rename(&legacy, &current) {
        eprintln!(
            "[ap] artifact: migrate legacy dir {} -> {}: {e}",
            legacy.display(),
            current.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Exports the graph at `graph_path` into `<repo_path>/.ai-architect-mcp-codebase/`.
///
/// Preconditions: `graph_path` exists (a closed lbug database — a directory in
/// lbug 0.15, but a single file is handled too) and the caller has already
/// dropped every open handle to it (so the snapshot is consistent). `node_count`
/// / `edge_count` are the counts the caller read from the just-built graph.
///
/// Postconditions on `Ok`: `graph.zst` (tar→zstd of the graph, written
/// atomically via a `.tmp` + rename) and `graph.meta.json` exist under the
/// artifact dir, and a `.gitattributes` `merge=ours` entry for the artifact
/// exists at the repo root. On `Err` nothing partial is left at the final
/// artifact path (the temp file is removed).
pub fn export_artifact(
    graph_path: &Path,
    repo_path: &Path,
    node_count: u64,
    edge_count: u64,
    manifest_path: Option<&Path>,
    coverage_path: Option<&Path>,
) -> Result<ExportStats, String> {
    migrate_legacy_dir(repo_path);
    if !graph_path.exists() {
        return Err(format!(
            "artifact export: graph path does not exist: {}",
            graph_path.display()
        ));
    }
    let dir = artifact_dir(repo_path);
    fs::create_dir_all(&dir)
        .map_err(|e| format!("artifact export: create {}: {e}", dir.display()))?;

    // Bundle the sidecars INTO the archive when they exist, so a fresh clone
    // inherits them on import:
    //   * the per-file manifest (issue #62) → classify the artifact→HEAD diff for
    //     the incremental fill without re-hashing the whole tree;
    //   * the coverage report (issue #57) → the bootstrapped clone starts with the
    //     exporter's honesty signal (which files were parse-incomplete/skipped).
    // Missing sidecars are not fatal — each importer degrades gracefully.
    let sidecars: Vec<(PathBuf, &str)> = [
        (manifest_path, ARTIFACT_MANIFEST_ENTRY),
        (coverage_path, ARTIFACT_COVERAGE_ENTRY),
    ]
    .into_iter()
    .filter_map(|(p, name)| p.filter(|p| p.exists()).map(|p| (p.to_path_buf(), name)))
    .collect();

    let level = ZSTD_LEVEL;
    let out = artifact_file(repo_path);
    let tmp = out.with_extension("zst.tmp");
    let compressed_bytes = write_archive(graph_path, &sidecars, &tmp, level).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    fs::rename(&tmp, &out).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("artifact export: rename into place: {e}")
    })?;

    let original_bytes = path_size(graph_path);
    let meta = ArtifactMeta {
        schema_version: SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        commit: git_head(repo_path).unwrap_or_default(),
        node_count,
        edge_count,
        original_bytes,
        compressed_bytes,
        compression_level: level,
    };
    write_meta(repo_path, &meta)?;
    ensure_gitattributes(repo_path);

    Ok(ExportStats {
        artifact_path: out,
        original_bytes,
        compressed_bytes,
    })
}

/// Streams `graph_path` (dir or single file) through tar → zstd into `dest`,
/// optionally bundling `manifest_path` as a top-level `file_manifest.json` entry.
/// Returns the compressed byte count. The archive stores the graph under its
/// own basename so import can unpack it back into `<parent>/<basename>`, and the
/// bundled manifest unpacks as a sibling `<parent>/file_manifest.json` —
/// precisely where the incremental classifier (issue #62) looks for it.
fn write_archive(
    graph_path: &Path,
    sidecars: &[(PathBuf, &str)],
    dest: &Path,
    level: i32,
) -> Result<u64, String> {
    let name = graph_path
        .file_name()
        .ok_or_else(|| "artifact export: graph path has no final component".to_string())?;
    let file = File::create(dest)
        .map_err(|e| format!("artifact export: create {}: {e}", dest.display()))?;
    let encoder =
        zstd::Encoder::new(file, level).map_err(|e| format!("artifact export: zstd init: {e}"))?;
    let mut builder = tar::Builder::new(encoder);

    if graph_path.is_dir() {
        builder
            .append_dir_all(name, graph_path)
            .map_err(|e| format!("artifact export: tar dir: {e}"))?;
    } else {
        builder
            .append_path_with_name(graph_path, name)
            .map_err(|e| format!("artifact export: tar file: {e}"))?;
    }
    for (path, entry_name) in sidecars {
        builder
            .append_path_with_name(path, entry_name)
            .map_err(|e| format!("artifact export: tar sidecar {entry_name}: {e}"))?;
    }
    let encoder = builder
        .into_inner()
        .map_err(|e| format!("artifact export: tar finish: {e}"))?;
    let file = encoder
        .finish()
        .map_err(|e| format!("artifact export: zstd finish: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("artifact export: fsync: {e}"))?;
    fs::metadata(dest)
        .map(|m| m.len())
        .map_err(|e| format!("artifact export: stat output: {e}"))
}

// ---------------------------------------------------------------------------
// Import (bootstrap)
// ---------------------------------------------------------------------------

/// Imports the committed artifact at `<repo_path>/.ai-architect-mcp-codebase/` into
/// `graph_path` (typically `<output_dir>/graph`).
///
/// Preconditions: `artifact_exists(repo_path)` is true and `graph_path` does
/// not yet exist (bootstrap only fills a missing local index — the caller
/// guarantees this). Postconditions on `Ok`: `graph_path` is a populated graph
/// equivalent to the one the artifact was exported from, and the returned
/// `ArtifactMeta` is the committed sidecar. On `Err`, `graph_path` is not left
/// as a partial graph directory (the unpack target is cleaned).
///
/// Errors loudly (never silently) so the caller can log the reason and fall
/// back to a full index explicitly.
pub fn import_artifact(repo_path: &Path, graph_path: &Path) -> Result<ArtifactMeta, String> {
    let meta = read_meta(repo_path)?;
    if meta.schema_version > SCHEMA_VERSION {
        return Err(format!(
            "artifact import: sidecar schema {} is newer than supported {} — refusing",
            meta.schema_version, SCHEMA_VERSION
        ));
    }
    let dest_parent = graph_path
        .parent()
        .ok_or_else(|| "artifact import: graph path has no parent".to_string())?;
    fs::create_dir_all(dest_parent)
        .map_err(|e| format!("artifact import: create {}: {e}", dest_parent.display()))?;

    let src = artifact_file(repo_path);
    let file =
        File::open(&src).map_err(|e| format!("artifact import: open {}: {e}", src.display()))?;
    let decoder =
        zstd::Decoder::new(file).map_err(|e| format!("artifact import: zstd init: {e}"))?;
    // Cap the decoded stream so a crafted artifact cannot exhaust disk.
    let mut archive = tar::Archive::new(decoder.take(MAX_DECOMPRESSED_BYTES));
    // tar-rs rejects absolute paths and `..` components on unpack by default,
    // so a malicious archive cannot escape `dest_parent` (path-traversal safe).
    archive.unpack(dest_parent).map_err(|e| {
        let _ = fs::remove_dir_all(graph_path);
        format!("artifact import: unpack: {e}")
    })?;

    if !graph_path.exists() {
        return Err(format!(
            "artifact import: archive did not produce expected graph at {}",
            graph_path.display()
        ));
    }
    Ok(meta)
}

/// Reads the committed sidecar for `repo_path`. Public so the bootstrap
/// composition root can inspect provenance (git sha) before deciding whether
/// to import.
pub fn read_artifact_meta(repo_path: &Path) -> Result<ArtifactMeta, String> {
    read_meta(repo_path)
}

/// Compares the artifact's provenance sha against the repo's current git HEAD.
///
/// Returns `None` when the artifact is FRESH — its sha equals HEAD, or the repo
/// is not a git working tree (HEAD unavailable), so staleness is not a
/// meaningful concept and a bootstrap is safe. Returns `Some(StaleInfo)` when
/// the artifact is provably not the current HEAD: either the shas differ, or the
/// artifact carries no provenance sha at all (empty) while the repo does have a
/// HEAD. `commits_behind` is computed via `git rev-list --count sha..HEAD` and
/// is `None` when the artifact sha is absent or unknown to this repo.
pub fn artifact_staleness(repo_path: &Path, artifact_sha: &str) -> Option<StaleInfo> {
    let head = git_head(repo_path)?; // not a git repo → treat as fresh
    if !artifact_sha.is_empty() && artifact_sha == head {
        return None;
    }
    let commits_behind = if is_hex_sha(artifact_sha) {
        commits_between(repo_path, artifact_sha, &head)
    } else {
        None // empty or non-sha provenance → count is unknowable
    };
    Some(StaleInfo {
        artifact_sha: artifact_sha.to_string(),
        head_sha: head,
        commits_behind,
    })
}

/// True when a committed, schema-compatible artifact is present in `repo_path`.
pub fn artifact_exists(repo_path: &Path) -> bool {
    migrate_legacy_dir(repo_path);
    let zst = artifact_file(repo_path);
    let present = fs::metadata(&zst).map(|m| m.len() > 0).unwrap_or(false);
    if !present {
        return false;
    }
    match read_meta(repo_path) {
        Ok(meta) => meta.schema_version <= SCHEMA_VERSION,
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Metadata sidecar
// ---------------------------------------------------------------------------

fn write_meta(repo_path: &Path, meta: &ArtifactMeta) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(meta)
        .map_err(|e| format!("artifact export: encode meta: {e}"))?;
    let path = meta_file(repo_path);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &json).map_err(|e| format!("artifact export: write meta: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("artifact export: rename meta: {e}")
    })
}

fn read_meta(repo_path: &Path) -> Result<ArtifactMeta, String> {
    let path = meta_file(repo_path);
    let bytes = fs::read(&path).map_err(|e| format!("artifact: read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("artifact: parse sidecar: {e}"))
}

// ---------------------------------------------------------------------------
// .gitattributes — prevent merge conflicts on the binary artifact
// ---------------------------------------------------------------------------

/// Ensures `<repo>/.gitattributes` carries a `merge=ours` entry for the
/// artifact, and best-effort configures the local `ours` merge driver (git has
/// no built-in one). Idempotent: the line is appended only if absent, so
/// re-export never duplicates it. Best-effort — a failure here degrades to
/// "the binary may conflict on merge", never a failed export.
// source: DeusData/codebase-memory-mcp src/pipeline/artifact.c ensure_gitattributes —
// `<artifact> binary merge=ours`, with `merge.ours.driver true` configured so
// the attribute resolves. `binary` must precede `merge=ours` only inside the
// same macro expansion; as separate tokens on one line, order is not load-bearing.
fn ensure_gitattributes(repo_path: &Path) {
    let entry = format!("{ARTIFACT_DIR}/{ARTIFACT_FILE} binary merge=ours");
    let ga = repo_path.join(".gitattributes");
    let existing = fs::read_to_string(&ga).unwrap_or_default();
    if !existing.lines().any(|l| l.trim() == entry) {
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str("# Auto-generated by AI Architect Codebase (issue #55):\n");
        next.push_str("# keep the committed graph snapshot from producing merge conflicts.\n");
        next.push_str(&entry);
        next.push('\n');
        if let Err(e) = fs::write(&ga, next) {
            eprintln!("[ap] artifact: write .gitattributes: {e}");
        }
    }
    // Best-effort local merge-driver registration. No shell — args are passed
    // directly to git, so no interpolation/injection surface.
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["config", "merge.ours.driver", "true"])
        .output();
}

// ---------------------------------------------------------------------------
// Directory size (best-effort, for the sidecar's original_bytes)
// ---------------------------------------------------------------------------

/// Sum of regular-file sizes under `path` (or the file's own size). Best-effort:
/// unreadable entries contribute 0. Used only for the informational
/// `original_bytes` sidecar field / compression-ratio logging.
fn path_size(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            total = total.saturating_add(path_size(&entry.path()));
        }
    }
    total
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
