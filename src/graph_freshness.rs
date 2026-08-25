//! Cheap query-time staleness guard for read-only graph tools (fleet-watch#112).
//!
//! Read tools open a graph built at some point in the past and never re-check
//! it against the working tree they still claim to describe — a stale answer
//! looks identical to a fresh one. This module gives that silent condition a
//! name: it compares the `file_manifest.json` sidecar's per-file
//! (mtime_ns, size) snapshot (recorded at index time, see `indexer::manifest`)
//! against the CURRENT state of those same files on disk, and — when the
//! indexed root is a git working tree — how many commits its HEAD has moved
//! since the index-time sha `write_graph_meta` stamped into `meta.json`.
//!
//! Cheap by construction: no re-hashing, no directory walk for files added
//! since the index (the incremental indexer's own hot-path change signal —
//! mtime OR size differs — is reused here, not a fuller and slower check).
//! Best-effort throughout: a missing or unreadable sidecar degrades to
//! `"unknown"` rather than failing or blocking the caller's query.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

use crate::artifact::{git_commits_between, git_head};
use crate::indexer::manifest::{self, FileState};

/// The subset of `meta.json` (written by `query_handlers::write_graph_meta`)
/// this check needs. `commit_sha` is absent on a schema-1 sidecar written
/// before fleet-watch#112 — `#[serde(default)]` degrades that case to `None`
/// (dirty-file signal only) instead of a parse failure.
#[derive(Deserialize)]
struct GraphMeta {
    root: String,
    #[serde(default)]
    commit_sha: Option<String>,
}

/// `graph_path` is `<output_dir>/graph`, already validated to exist by the
/// caller — `search_codebase`/`get_symbol`/`get_impact` all check this before
/// opening the store. Reads `meta.json` and `file_manifest.json`, both
/// siblings of `graph` in `output_dir`.
pub(crate) fn check(graph_path: &Path) -> Value {
    let Some(output_dir) = graph_path.parent() else {
        return unknown();
    };
    let Some(meta) = read_meta(output_dir) else {
        return unknown();
    };
    let Some(loaded) = manifest::load(&manifest::manifest_path(output_dir)) else {
        return unknown();
    };

    let root = Path::new(&meta.root);
    let dirty_files = count_dirty(root, &loaded.files);
    let commits_behind = meta.commit_sha.as_deref().and_then(|indexed| {
        let head = git_head(root)?;
        if indexed == head {
            Some(0)
        } else {
            git_commits_between(root, indexed, &head)
        }
    });

    json!({
        "state": if dirty_files == 0 { "fresh" } else { "stale" },
        "checked_files": loaded.files.len(),
        "dirty_files": dirty_files,
        "commits_behind": commits_behind,
    })
}

fn unknown() -> Value {
    json!({ "state": "unknown" })
}

fn read_meta(output_dir: &Path) -> Option<GraphMeta> {
    let bytes = std::fs::read(output_dir.join("meta.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Re-stats every manifested file; counts one "dirty" per file whose current
/// (mtime_ns, size) no longer matches what was recorded at index time,
/// INCLUDING a file that has since been deleted. A file added since the index
/// is not visible here — no manifest entry to compare against — a directory
/// walk would make this check pay for the whole tree on every read-only call,
/// defeating "cheap". That gap makes this signal a floor on staleness, not a
/// ceiling: a caller who needs the ceiling forces `full=true` on
/// `index_codebase` rather than trusting this guard alone.
fn count_dirty(root: &Path, files: &BTreeMap<String, FileState>) -> usize {
    files
        .iter()
        .filter(|(rel, state)| match std::fs::metadata(root.join(rel)) {
            Ok(m) => manifest::mtime_ns(&m) != state.mtime_ns || m.len() != state.size,
            Err(_) => true,
        })
        .count()
}

#[cfg(test)]
#[path = "graph_freshness_tests.rs"]
mod tests;
