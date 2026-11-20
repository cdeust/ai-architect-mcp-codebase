//! The record a completed `analyze_codebase` leaves, so the next call on an
//! unchanged tree can skip resolve, cluster and the search index (issue #443).
//!
//! # Why
//!
//! A refresh that re-parses nothing still ran `resolve_graph`,
//! `cluster_graph` and `build_search_index` over the whole graph: the second
//! call measured 4.76x faster than the first on ripgrep and 2.28x on this
//! repository's `src/`, resolve and cluster being 65% and 98% of it
//! (`benchmarks/incremental_speed`, `second_call`, #579). The issue asks for 5x.
//!
//! # When the later phases are skipped
//!
//! All of these must hold, otherwise every phase runs as before:
//! 1. `<output_dir>/analysis_stamp.json` loads. A completed analysis writes it
//!    last, after the graph handle is closed; every call removes it before
//!    phase 1 writes anything, so a call that fails or is killed leaves none;
//! 2. it was written by this server version, with this call's `resolution`
//!    (`gamma`), and the language-server pass did not run (`lsp: true`
//!    rebuilds in full anyway, `index_phase::reusable_manifest`);
//! 3. the graph and the search index hold, byte for byte, what that analysis
//!    left (`fingerprint`, a SHA-256 over every file's relative path, length
//!    and content). Any other writer in between, `index_codebase`,
//!    `resolve_graph`, `cluster_graph`, `ingest_traces` or `lsp_resolve`,
//!    changes a file, so its rows are never reported under a stale count;
//! 4. phase 1 took the incremental path and found no file changed, added,
//!    deleted or renamed (`IndexPhase::tree_unchanged`);
//! 5. the Cargo facts that pass wrote again equal the ones the analysis
//!    recorded (`CargoFacts::digest`). They are computed over the whole tree
//!    and read by the resolver (issues #353 and #358), and a `Cargo.toml` a
//!    `language: "rust"` walk does not list can move them.
//!
//! The graph is then the one the previous analysis finished, so resolve,
//! cluster and the search index would decide the same rows; the response
//! repeats that analysis's `resolve`, `cluster` and `search_index` blocks
//! (`search_index.elapsed_ms` included) and says so in `phases_reused`.

use super::AnalyzeRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// File name of the stamp under `output_dir`.
pub(super) const STAMP_FILE: &str = "analysis_stamp.json";

/// The stamp holds three small JSON blocks and four short strings: well
/// under a kilobyte. 1 MiB is a fail-safe bound; over it the stamp is ignored.
const STAMP_MAX_BYTES: u64 = 1 << 20;

/// The response blocks of the phases a reuse skips.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(super) struct LaterPhases {
    pub(super) resolve: Value,
    pub(super) cluster: Value,
    pub(super) search_index: Value,
}

#[derive(Serialize, Deserialize)]
struct Stamp {
    version: String,
    /// `f64::to_bits` of the `resolution` the clustering ran with, so the
    /// comparison is exact.
    gamma_bits: u64,
    cargo_digest: String,
    fingerprint: String,
    phases: LaterPhases,
}

/// A stamp that passed conditions 1 to 3, waiting for phase 1's verdict.
pub(super) struct Candidate(Stamp);

impl Candidate {
    /// Conditions 4 and 5: the blocks to repeat, or `None` to run the phases.
    pub(super) fn reusable(self, index: &super::index_phase::IndexPhase) -> Option<LaterPhases> {
        (index.tree_unchanged && index.cargo_digest == self.0.cargo_digest).then_some(self.0.phases)
    }
}

fn stamp_path(output_dir: &Path) -> PathBuf {
    output_dir.join(STAMP_FILE)
}

/// Removes the stamp and returns it when conditions 1 to 3 hold. Called
/// before phase 1 writes anything. A stamp that cannot be removed is not
/// used: it would outlive a call that changes the graph and then fails.
pub(super) fn take(req: &AnalyzeRequest) -> Option<Candidate> {
    let path = stamp_path(&req.output_dir);
    let loaded = crate::indexer::manifest::read_regular_capped(&path, STAMP_MAX_BYTES)
        .and_then(|(bytes, _)| serde_json::from_slice::<Stamp>(&bytes).ok());
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("[ap] analyze_codebase: analysis stamp not removed ({e}); not reused");
            return None;
        }
    }
    let stamp = loaded?;
    if stamp.version != env!("CARGO_PKG_VERSION") || stamp.gamma_bits != req.gamma.to_bits() {
        return None;
    }
    if fingerprint(req).as_deref() != Some(stamp.fingerprint.as_str()) {
        eprintln!(
            "[ap] analyze_codebase: the graph or search index changed since the last \
             analysis; running every phase"
        );
        return None;
    }
    Some(Candidate(stamp))
}

/// Writes the stamp of a completed analysis, best-effort: a stamp that is
/// missing only costs the next call its skip. Call it after the graph handle
/// of the analysis is closed, so the fingerprint is the graph at rest.
pub(super) fn record(req: &AnalyzeRequest, cargo_digest: &str, phases: LaterPhases) {
    let Some(fingerprint) = fingerprint(req) else {
        eprintln!("[ap] analyze_codebase: graph not fingerprinted; no analysis stamp");
        return;
    };
    let stamp = Stamp {
        version: env!("CARGO_PKG_VERSION").to_string(),
        gamma_bits: req.gamma.to_bits(),
        cargo_digest: cargo_digest.to_string(),
        fingerprint,
        phases,
    };
    if let Err(e) = crate::atomic_file::write_json_atomic(&stamp_path(&req.output_dir), &stamp) {
        eprintln!("[ap] analyze_codebase: analysis stamp not written: {e}");
    }
}

/// SHA-256 over the graph and the search index: for each regular file, in
/// path order, its path relative to `output_dir`, its length and the SHA-256
/// of its bytes. `None` when either is missing, holds a symlink or anything
/// other than files and directories, or cannot be read: such a graph is never
/// vouched for.
fn fingerprint(req: &AnalyzeRequest) -> Option<String> {
    let mut files = Vec::new();
    for root in [
        req.graph_dir.clone(),
        req.output_dir.join(crate::search::SEARCH_INDEX_DIR),
    ] {
        collect_files(&root, &mut files)?;
    }
    files.sort();
    let mut records = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&req.output_dir).unwrap_or(path);
        let (len, digest) = hash_stream(path)?;
        records.extend_from_slice(rel.to_string_lossy().as_bytes());
        records.push(0);
        records.extend_from_slice(&len.to_le_bytes());
        records.extend_from_slice(&digest);
    }
    Some(crate::indexer::manifest::hash_bytes(&records))
}

/// Every regular file at or under `path`; `None` on a symlink, a special
/// file, a missing `path` or an unreadable directory.
fn collect_files(path: &Path, out: &mut Vec<PathBuf>) -> Option<()> {
    let kind = std::fs::symlink_metadata(path).ok()?.file_type();
    if kind.is_file() {
        out.push(path.to_path_buf());
        return Some(());
    }
    if !kind.is_dir() {
        return None;
    }
    for entry in std::fs::read_dir(path).ok()? {
        collect_files(&entry.ok()?.path(), out)?;
    }
    Some(())
}

/// Length and SHA-256 of a file, read in chunks: a graph can be over a
/// gigabyte (django, issue #448).
fn hash_stream(path: &Path) -> Option<(u64, Vec<u8>)> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Some((len, hasher.finalize().to_vec()))
}
