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
use std::path::{Component, Path};

use std::process::Command;

use crate::git_provenance::{git_head, is_hex_sha};
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

/// The response key the read tools carry this receipt under.
///
/// Deliberately NOT `graph_state`: that key is already taken elsewhere in this
/// server, where `index_codebase` / `index_history` set it to a plain STRING
/// (`"fresh"`, `"accepted_stale"`, `"filled_to_working_tree"` — see
/// `history_handlers`). Publishing a nested OBJECT under the same name would
/// give one key two incompatible shapes across the server's tools, so any
/// client that types the field once breaks on whichever tool it typed second
/// (fleet-watch#112 review). The string field keeps its name and its meaning;
/// the new object gets its own.
pub(crate) const RESPONSE_KEY: &str = "graph_freshness";

/// Stamps the freshness receipt onto a response envelope under `RESPONSE_KEY`.
///
/// `response` is a response envelope — a JSON object — at every call site; a
/// value with no keys has nowhere to carry a receipt and is left untouched.
fn attach(response: &mut Value, graph_path: &Path) {
    if let Some(envelope) = response.as_object_mut() {
        envelope.insert(RESPONSE_KEY.to_string(), check(graph_path));
    }
}

/// Stamps the receipt onto a read tool's response, taking the graph from the
/// tool's own `arguments`.
///
/// EVERY exit of a read tool must carry the receipt, not just the ones that
/// succeed: a caller told `symbol_not_found` is the caller who most needs to
/// know the graph may simply predate the symbol, and a caller told the store
/// could not be opened or the query failed is looking at the exact signature an
/// in-progress re-index produces.
///
/// This is called from the `run_*` wrapper of each instrumented tool, and
/// nowhere else. That wrapper is the tool's ONLY exit — `main.rs` dispatches to
/// `run_get_symbol` / `run_get_impact` / `run_search_codebase`, each of which
/// funnels both the `Ok` value and the `Err`-turned-envelope through one point.
/// Attaching there is what makes the guarantee structural rather than a matter
/// of remembering: an earlier revision attached at each named `return` inside
/// the `do_*` body, which covered the exits it could see and silently missed
/// every `?`-propagated failure before them — `graph_cache::open_cached(..)?`
/// and the `find_symbol_*(..)?` calls (fleet-watch#112 review round 3). A `?`
/// added anywhere in a `do_*` body in future is covered by construction.
///
/// No receipt when `arguments` names no `graph_path`: the call was rejected
/// before any graph was identified, so there is no graph whose freshness could
/// be reported. A `graph_path` that does not exist or cannot be read yields the
/// receipt's own `"unknown"` state rather than silence.
pub(crate) fn attach_from_arguments(response: &mut Value, arguments: &Value) {
    let Some(graph_str) = arguments.get("graph_path").and_then(|v| v.as_str()) else {
        return;
    };
    attach(response, Path::new(graph_str));
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
///
/// COST — `O(manifested files)` `stat(2)` calls on EVERY call, with no cache
/// and no sampling. `search_codebase`, `get_symbol` and `get_impact` each pay
/// it once per invocation, so the price of a read tool grows linearly with the
/// size of the indexed codebase, not with the size of the answer. Concretely,
/// a repository with 50k indexed files pays 50k `stat`s per read-tool call; a
/// warm page cache makes each one cheap (sub-microsecond), but 50k of them are
/// still tens of milliseconds, and a cold cache or a network filesystem is far
/// worse. "Cheap" here means cheap RELATIVE to re-hashing or re-walking the
/// tree — it is not a constant-time check, and a caller driving these tools in
/// a tight loop over a large codebase should expect the guard to dominate. No
/// bound is imposed deliberately: a sampled or time-boxed check would report a
/// freshness verdict it cannot stand behind, which is the exact failure this
/// module exists to remove.
fn count_dirty(root: &Path, files: &BTreeMap<String, FileState>) -> usize {
    files
        .iter()
        .filter(|(rel, state)| {
            if !is_contained_key(rel) {
                // Never stat it. Counted dirty rather than skipped: a manifest
                // we cannot verify against the tree is not evidence the graph
                // is fresh.
                return true;
            }
            match std::fs::metadata(root.join(rel)) {
                Ok(m) => manifest::mtime_ns(&m) != state.mtime_ns || m.len() != state.size,
                Err(_) => true,
            }
        })
        .count()
}

/// True when `rel` is a manifest key this check may resolve against the indexed
/// root — i.e. a relative path made only of ordinary components.
///
/// `file_manifest.json` is an on-disk sidecar a caller can craft, the same
/// threat model `commit_sha` is guarded against by `is_hex_sha` above. The
/// hazard here is `Path::join`, which does not constrain what it is given:
/// handed an ABSOLUTE component it discards the base entirely — verified,
/// `Path::new("/root/base").join("/etc/hosts")` is `/etc/hosts` — and a `..`
/// component walks out of the tree. Either way the `stat` that follows reads a
/// file the caller never indexed, and the dirty count then reports whether that
/// file's (mtime_ns, size) matched the value the attacker put in the manifest.
/// That is an existence-and-attributes oracle over the whole host filesystem,
/// answered through a read-only tool's response, one guess per call.
///
/// The indexer writes keys by stripping the root prefix
/// (`indexer::incremental::strip_prefix_path`), so every legitimate key is
/// already a relative path of ordinary components. Requiring exactly that is
/// both the real invariant and the cheapest possible check — no extra syscall
/// on a path that already costs one `stat` per entry, so the documented
/// `O(manifested files)` cost is unchanged.
///
/// Lexical containment only. A symlink INSIDE the indexed tree that points out
/// of it still resolves outward; that is a property of the tree the caller
/// asked to index and which the indexer itself already followed, not something
/// the sidecar controls. Canonicalising per entry would add a syscall per file
/// to defend a different threat, and is deliberately not done here.
fn is_contained_key(rel: &str) -> bool {
    !rel.is_empty()
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
#[path = "graph_freshness_tests.rs"]
mod tests;
