//! Cheap query-time staleness guard for read-only graph tools (fleet-watch#112).
//!
//! Read tools open a graph built at some point in the past and never re-check
//! it against the working tree they still claim to describe — a stale answer
//! looks identical to a fresh one. This module gives that silent condition a
//! name: it compares the `file_manifest.json` sidecar's per-file
//! (mtime_ns, size) snapshot (recorded at index time, see `indexer::manifest`)
//! against the CURRENT state of those same files on disk, and — when the
//! indexed root is a git working tree — how far its HEAD has moved, in EITHER
//! direction, from the index-time sha `write_graph_meta` stamped into
//! `meta.json`.
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

use std::process::Command;

use crate::artifact::{git_head, is_hex_sha};
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
    let divergence = commit_divergence(root, meta.commit_sha.as_deref());
    // A HEAD that sits on a different commit than the one indexed is staleness
    // the file check cannot see: a commit that only ADDS files leaves every
    // manifested file byte-identical, and `count_dirty` has no entry to compare
    // an added file against. Either direction counts — moved forward, moved
    // back, or diverged onto another branch — so the verdict is a plain OR over
    // the two signals, not a special case per direction.
    let head_moved = divergence.is_some_and(|(ahead, behind)| ahead > 0 || behind > 0);

    json!({
        "state": if dirty_files == 0 && !head_moved { "fresh" } else { "stale" },
        "checked_files": loaded.files.len(),
        "dirty_files": dirty_files,
        "commits_behind": divergence.map(|(_, behind)| behind),
        "commits_ahead": divergence.map(|(ahead, _)| ahead),
    })
}

/// Commits separating the index-time sha from the root's CURRENT HEAD, as
/// `(ahead, behind)`. `behind` counts commits HEAD has that the indexed commit
/// does not; `ahead` counts commits the indexed commit has that HEAD does not —
/// HEAD moved BACKWARD (a checkout to an older commit) or onto a divergent
/// branch.
///
/// Both directions are load-bearing because either alone lies. `git rev-list
/// --count indexed..head` — the one-directional form — returns 0 whenever HEAD
/// is an ancestor of the indexed commit, which is indistinguishable from "HEAD
/// IS the indexed commit" (measured 2026-08-25: checking out one commit back
/// gives 0 for `indexed..head` and 1 for `head..indexed`). One
/// `--left-right --count indexed...head` answers both, so the bidirectional
/// check costs the same single `git` spawn the one-directional one did. For
/// `A...B` git prints the A-only count first, then the B-only count — here,
/// ahead then behind.
///
/// `is_hex_sha` guards `indexed` before it reaches git: the value is read from
/// an on-disk sidecar a caller can craft, so unguarded it is EVALUATED as a
/// revision expression rather than compared as an object id, and a `-`-leading
/// value smuggles a flag into `git rev-list` (arg injection, not shell
/// injection). Same guard `artifact::artifact_staleness` applies to the same
/// command.
///
/// `None` — the sidecar carries no usable provenance and the commit signal is
/// simply absent — when `indexed` is missing (a schema-1 sidecar) or not a
/// plausible object sha, when the root is not a git working tree, or when git
/// cannot resolve the range (e.g. the indexed commit is unknown to this clone).
fn commit_divergence(root: &Path, indexed: Option<&str>) -> Option<(u64, u64)> {
    let indexed = indexed.filter(|sha| is_hex_sha(sha))?;
    let head = git_head(root)?;
    if indexed == head {
        return Some((0, 0));
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--left-right", "--count"])
        .arg(format!("{indexed}...{head}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut counts = text.split_whitespace();
    let ahead = counts.next()?.parse::<u64>().ok()?;
    let behind = counts.next()?.parse::<u64>().ok()?;
    Some((ahead, behind))
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
