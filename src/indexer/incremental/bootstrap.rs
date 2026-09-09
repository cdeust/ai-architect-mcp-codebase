// bootstrap — artifact-bootstrap incremental fill (issue #55 completion).
//
// Fills a just-imported artifact graph up to the current working tree by
// classifying the artifact→working-tree diff (git diff when available,
// manifest content-hash fallback otherwise) and running it through the same
// `apply_changes` core `index_incremental` (in the parent module) uses.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use crate::graph_store::GraphStore;

use super::super::manifest::{self, FileManifest, FileState};
use super::super::walk::WalkOptions;
use super::classify::{classify, discover};
use super::{apply_changes, ChangeSet, Discovered, Rename};
use super::{coverage::save_incremental_coverage, IncrementalResult, IndexOptions};

/// How the bootstrap fill classified the diff — surfaced in the tool response so
/// the caller always knows which signal drove the fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillMethod {
    /// `git diff <artifact_sha> <working tree>` — the precise, rename-aware path.
    GitDiff,
    /// Manifest content-hash classification — the fallback when git cannot diff
    /// (not a git tree, or the artifact sha is unknown to this clone).
    ContentHash,
}

/// The result of an artifact-bootstrap incremental fill: the same change
/// partition an incremental re-index reports, plus the method used.
pub struct FillResult {
    pub result: IncrementalResult,
    pub method: FillMethod,
}

/// Fills the just-bootstrapped graph at `graph_dir` (imported from an artifact
/// exported at `artifact_sha`) up to the current working tree, re-parsing only
/// the files that changed since `artifact_sha`.
///
/// Preconditions: `graph_dir` holds the freshly imported artifact graph (as-of
/// `artifact_sha`); `imported_manifest` is the manifest bundled in that artifact
/// (or `None` if the artifact carried none). Postconditions on `Ok`: the graph
/// equals what a full index of the current tree would produce at the index
/// stage, a fresh local manifest reflecting the current tree is written to
/// `manifest_path`, and the returned `FillResult` reports the change partition
/// and the classification method. Never a silent path — the caller reports the
/// counts and method so no one mistakes a filled graph for a cold one.
///
/// Classification: primary is `git diff <artifact_sha>` against the working tree
/// (committed AND uncommitted, renames via `-M`, untracked via `ls-files`).
/// Fallback when git cannot diff (not a git tree / unknown sha): the imported
/// manifest's content hashes. If neither is available, returns `Err` so the
/// caller falls back to a full index explicitly.
pub fn fill_after_bootstrap(
    codebase: &Path,
    graph_dir: &Path,
    manifest_path: &Path,
    artifact_sha: &str,
    imported_manifest: Option<&FileManifest>,
    options: &IndexOptions,
) -> Result<FillResult, String> {
    let start = Instant::now();
    let dependency_scope = options.dependency_scope;
    // No create_schema(): the imported artifact graph already carries the schema.
    let store = GraphStore::open_or_create(graph_dir)?;
    store.require_entry_metadata()?;
    let walk_opts = WalkOptions {
        language_filter: options.language_filter,
        dependency_scope,
        exclude_dirs: options.exclude_dirs.clone(),
    };
    let (current, walk_gaps) = discover(codebase, walk_opts)?;

    // Choose the classification signal. git diff is precise and cheap on a fresh
    // clone (no mtimes to trust); the manifest hash fallback is correct but scans
    // the tree, so it is used only when git is unavailable.
    let (changes, unchanged_count, next_manifest, method) =
        match git_changes(codebase, artifact_sha, &current) {
            Some(cs) => {
                let reparsed = cs.changed.len() + cs.added.len() + cs.renamed.len();
                let unchanged = current.len().saturating_sub(reparsed);
                let manifest = manifest_from_discovered(&current);
                (cs, unchanged as u64, manifest, FillMethod::GitDiff)
            }
            None => {
                let prior = imported_manifest.ok_or_else(|| {
                    "artifact fill: cannot diff (git unavailable AND no bundled manifest)"
                        .to_string()
                })?;
                let plan = classify(prior, &current);
                let unchanged = plan.unchanged.len() as u64;
                (
                    plan.change_set,
                    unchanged,
                    plan.next_manifest,
                    FillMethod::ContentHash,
                )
            }
        };

    let changed = changes.changed.len() as u64;
    let added = changes.added.len() as u64;
    let deleted = changes.deleted.len() as u64;
    let renamed = changes.renamed.len() as u64;

    let (reparsed, reparsed_gaps) =
        apply_changes(&store, codebase, &changes, &current, dependency_scope)?;
    // Persist a local manifest reflecting the CURRENT tree so subsequent local
    // incrementals classify against this machine's mtimes/hashes, not the
    // artifact's (whose mtimes are meaningless on a fresh clone).
    manifest::save(manifest_path, &next_manifest)?;

    // Coverage (issue #57/#249): the artifact bundled the exporter's coverage
    // (loaded from the sidecar unpacked beside the graph). Carry forward the
    // files the fill did not touch, overlay the reparsed files' fresh gaps
    // plus this pass's walk-level excluded/unreadable directories, save.
    let mut merged_gaps = reparsed_gaps;
    merged_gaps.extend(walk_gaps);
    save_incremental_coverage(
        codebase,
        graph_dir,
        &current,
        &changes,
        merged_gaps,
        "bootstrap_fill",
    );

    Ok(FillResult {
        result: IncrementalResult {
            graph_path: graph_dir.to_path_buf(),
            changed,
            added,
            deleted,
            renamed,
            unchanged: unchanged_count,
            files_reparsed: reparsed,
            elapsed_ms: start.elapsed().as_millis() as u64,
        },
        method,
    })
}

/// Builds a `FileManifest` for the current tree from an already-completed
/// discovery, hashing each file's bytes. Avoids a second walk (we already have
/// the paths + sizes); the hash is the only thing not carried by `Discovered`.
fn manifest_from_discovered(current: &[Discovered]) -> FileManifest {
    let mut m = FileManifest::new();
    for d in current {
        m.files.insert(
            d.rel.clone(),
            FileState {
                mtime_ns: d.mtime_ns,
                size: d.size,
                content_hash: manifest::hash_file(&d.abs).unwrap_or_default(),
            },
        );
    }
    m
}

/// Shared read-only context `collect_tracked_changes` and
/// `collect_untracked_adds` both need: the codebase root (to shell out to
/// git), the indexed-subtree prefix (to relativize git's paths), and the
/// rel→Discovered lookup (to map a git path to its working-tree metadata).
/// Bundled per §4.4 (param-object remedy — coding-standards.md, the same
/// pattern this repo already uses elsewhere for >4-parameter siblings):
/// `git_changes` builds one and passes it to both collectors instead of
/// each threading the same three values individually.
struct DiffContext<'a> {
    codebase: &'a Path,
    prefix: &'a str,
    by_rel: &'a HashMap<&'a str, &'a Discovered>,
}

/// Classifies the artifact→working-tree diff with git, or `None` when git cannot
/// be used (not a git working tree, or `artifact_sha` is unknown to this clone —
/// e.g. a shallow clone). `current` is the working-tree discovery, used to map
/// git paths to indexed File ids and to fetch (mtime, size) for re-parse.
///
/// Orchestrates the three phases (Fowler "Extract Function", §4.2 — this used
/// to be one 66-line function): setup/validation below, then
/// `collect_tracked_changes` for the committed+uncommitted tracked diff, then
/// `collect_untracked_adds` for new-but-never-`git add`-ed files. Paths are
/// made relative to the indexed subtree via `--show-prefix`; anything outside
/// it is ignored (both collectors read `ctx.prefix` for this).
fn git_changes(codebase: &Path, artifact_sha: &str, current: &[Discovered]) -> Option<ChangeSet> {
    if !is_hex_sha(artifact_sha) {
        return None;
    }
    // The indexed tree's path within the git repo (empty when codebase is the
    // repo root). `None` → not a git working tree → caller uses the fallback.
    let prefix = git_output(codebase, &["rev-parse", "--show-prefix"])?;
    let prefix = prefix.trim().to_string();
    // The artifact's commit must be present in this clone to diff against it.
    git_output(
        codebase,
        &["cat-file", "-e", &format!("{artifact_sha}^{{commit}}")],
    )?;

    let by_rel: HashMap<&str, &Discovered> = current.iter().map(|d| (d.rel.as_str(), d)).collect();
    let ctx = DiffContext {
        codebase,
        prefix: &prefix,
        by_rel: &by_rel,
    };
    let mut cs = ChangeSet::default();

    collect_tracked_changes(&ctx, artifact_sha, &mut cs);
    collect_untracked_adds(&ctx, &mut cs);

    Some(cs)
}

/// Fills `cs` with committed + uncommitted TRACKED changes: artifact commit
/// vs working tree. Uses `git diff <artifact_sha>` (artifact commit → WORKING
/// TREE, not just HEAD) so both committed and uncommitted tracked changes are
/// captured; rename detection is git's own `-M`. A `None` from `git_output`
/// (git failure) leaves `cs` untouched rather than erroring — `git_changes`
/// as a whole has already committed to `Some` by this point.
fn collect_tracked_changes(ctx: &DiffContext, artifact_sha: &str, cs: &mut ChangeSet) {
    let Some(diff) = git_output(
        ctx.codebase,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-status",
            "-M",
            artifact_sha,
            "--",
        ],
    ) else {
        return;
    };
    for line in diff.lines() {
        parse_diff_line(line, ctx.prefix, ctx.by_rel, cs);
    }
}

/// Fills `cs` with UNTRACKED new files (never `git add`-ed) — these are
/// invisible to `git diff`, so they're added here as `cs.added` entries to
/// make sure an uncommitted new file still gets (re)parsed. Dedups against
/// what `collect_tracked_changes` already recorded (added/changed) so a file
/// that appears in both listings — e.g. a rename target — isn't double-added.
fn collect_untracked_adds(ctx: &DiffContext, cs: &mut ChangeSet) {
    let Some(others) = git_output(
        ctx.codebase,
        &[
            "-c",
            "core.quotepath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
        ],
    ) else {
        return;
    };
    let already: HashSet<String> = cs
        .added
        .iter()
        .map(|d| d.rel.clone())
        .chain(cs.changed.iter().map(|d| d.rel.clone()))
        .collect();
    for line in others.lines() {
        let Some(rel) = strip_prefix_path(line.trim(), ctx.prefix) else {
            continue;
        };
        if already.contains(&rel) {
            continue;
        }
        if let Some(d) = ctx.by_rel.get(rel.as_str()) {
            cs.added.push((*d).clone());
        }
    }
}

/// Parses one `git diff --name-status -M` line into `cs`. Statuses: `M`/`T`
/// (modified/type-changed) → changed; `A`/`C` (added/copied) → added; `D` →
/// deleted; `R` (renamed) → renamed. Paths outside the indexed subtree, or
/// targets the indexer doesn't visit, are skipped (a full index wouldn't include
/// them either).
fn parse_diff_line(
    line: &str,
    prefix: &str,
    by_rel: &HashMap<&str, &Discovered>,
    cs: &mut ChangeSet,
) {
    let mut fields = line.split('\t');
    let status = match fields.next() {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };
    let code = status.as_bytes()[0];
    match code {
        b'R' | b'C' => {
            let old = fields.next();
            let new = fields.next();
            let (old, new) = match (old, new) {
                (Some(o), Some(n)) => (o, n),
                _ => return,
            };
            let new_rel = match strip_prefix_path(new, prefix) {
                Some(r) => r,
                None => return, // new path outside the indexed subtree
            };
            match by_rel.get(new_rel.as_str()) {
                Some(d) => {
                    if code == b'R' {
                        if let Some(old_rel) = strip_prefix_path(old, prefix) {
                            cs.renamed.push(Rename {
                                old_rel,
                                new_file: (*d).clone(),
                            });
                        } else {
                            cs.added.push((*d).clone());
                        }
                    } else {
                        cs.added.push((*d).clone());
                    }
                }
                None => {
                    // New end isn't indexed; drop the old end if this was a rename.
                    if code == b'R' {
                        if let Some(old_rel) = strip_prefix_path(old, prefix) {
                            cs.deleted.push(old_rel);
                        }
                    }
                }
            }
        }
        _ => {
            let path = match fields.next() {
                Some(p) => p,
                None => return,
            };
            let rel = match strip_prefix_path(path, prefix) {
                Some(r) => r,
                None => return,
            };
            match code {
                b'D' => cs.deleted.push(rel),
                b'A' => {
                    if let Some(d) = by_rel.get(rel.as_str()) {
                        cs.added.push((*d).clone());
                    }
                }
                // M, T, and any other in-place edit → changed.
                _ => {
                    if let Some(d) = by_rel.get(rel.as_str()) {
                        cs.changed.push((*d).clone());
                    }
                }
            }
        }
    }
}

/// Strips the repo-subtree `prefix` from a repo-relative git path, yielding the
/// indexed File id (forward-slash). Returns `None` when the path is outside the
/// indexed subtree.
fn strip_prefix_path(path: &str, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return Some(path.to_string());
    }
    path.strip_prefix(prefix).map(|s| s.to_string())
}

/// Runs `git -C <codebase> <args>` and returns trimmed stdout on success, or
/// `None` on any failure (spawn error, non-zero exit). No shell — args are
/// passed directly (injection-safe, matching `artifact.rs`).
fn git_output(codebase: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(codebase)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// True for a plausible git object sha (non-empty ASCII hex) — guards the sha
/// against `git` arg-injection, mirroring `artifact::is_hex_sha`.
fn is_hex_sha(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}
