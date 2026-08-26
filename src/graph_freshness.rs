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
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

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
    /// The size and mtime of the `file_manifest.json` this sidecar was written
    /// to accompany. Absent on a sidecar written before schema 3 — and absent
    /// is not a pass, see `describes_manifest_at`.
    ///
    /// `schema_version` is deliberately NOT read. It is a number the sidecar
    /// declares about itself, and this module's whole threat model is that the
    /// sidecar is attacker-writable, so a version field can only ever say what
    /// the writer wants it to say (fleet-watch#112 review round 6).
    #[serde(default)]
    manifest_size: Option<u64>,
    #[serde(default)]
    manifest_mtime_ns: Option<i64>,
}

impl GraphMeta {
    /// True when the manifest on disk is the one this `meta.json` was written
    /// for — i.e. the two sidecars are a matching pair rather than two halves of
    /// different indexes.
    ///
    /// Both indexing paths now write the manifest FIRST and `meta.json` LAST, so
    /// `meta.json` is the commit point of an index and names the manifest it
    /// belongs to. A reader that lands mid-index therefore sees the OLD meta
    /// beside the NEW manifest, and the recorded identity no longer matches —
    /// which is the signal, and the reason this is checked after `manifest::load`
    /// rather than before: a manifest swapped between the load and this stat is
    /// caught by the same comparison (fleet-watch#112 review round 4).
    ///
    /// A schema-1/2 sidecar carries no identity to compare, so it pairs by
    /// default — an old sidecar degrades to the previous behaviour instead of
    /// reading as permanently torn.
    fn describes_manifest(&self, size: u64, mtime_ns: i64) -> bool {
        let (Some(claimed_size), Some(claimed_mtime)) =
            (self.manifest_size, self.manifest_mtime_ns)
        else {
            // No identity recorded. Two ways to get here and both mean the same
            // thing — this sidecar cannot be shown to belong to that manifest:
            // a pre-schema-3 sidecar that never recorded one, or a sidecar
            // written BEFORE its manifest, which is the ordering bug itself.
            //
            // Previously a sidecar could opt OUT of this check by declaring
            // `"schema_version": 2`, so a forged `meta.json` carrying nothing
            // but a root and a commit_sha bypassed the whole pairing defence
            // with one field and no race at all (fleet-watch#112 review round
            // 6). The version is no longer consulted: absence of identity is a
            // failure to pair, whatever the sidecar says about itself.
            //
            // Cost of the strictness: a graph indexed by an older build reads
            // as `"unknown"` until it is re-indexed. That is the honest answer
            // — the pair genuinely cannot be verified — and it is the safe
            // direction, unlike a bypass that reports `"fresh"`.
            return false;
        };
        claimed_size == size && claimed_mtime == mtime_ns
    }
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
    let Some(inputs) = admissible_inputs(graph_path) else {
        return unknown();
    };
    verdict(&inputs)
}

/// The three things a freshness verdict is computed from, once every gate that
/// could disqualify them has passed.
struct CheckInputs {
    root: PathBuf,
    files: BTreeMap<String, FileState>,
    commit_sha: Option<String>,
}

/// Every reason this check can decline to answer, in one place.
///
/// `None` means "no verdict is available", which the caller reports as
/// `"unknown"`. Split out of `check` so the gates read as the list they are
/// (fleet-watch#112 review round 5) and so each one can be given its reason
/// without burying the two-line computation that follows them.
fn admissible_inputs(graph_path: &Path) -> Option<CheckInputs> {
    // The graph artifact itself must be there. Without this the receipt happily
    // reports "fresh" beside a `status: "error"` envelope when the graph was
    // deleted and its sidecars left behind — the exact contradiction this
    // receipt exists to prevent. The read tools gate on the same condition and
    // return `Err` first, but `run_*` stamps the receipt onto that error
    // envelope regardless (round 4).
    if !graph_path.exists() {
        return None;
    }
    let output_dir = graph_path.parent()?;
    let meta = read_meta(output_dir)?;
    let root = validated_root(&meta.root)?;
    // Content AND identity from a single open handle. Reading the manifest and
    // then stat-ing the path separately samples the name twice, so a rewrite
    // between the two reads pairs one index's bytes with another's identity —
    // a narrower race than the one below, but the same class (round 6).
    let (loaded, size, mtime_ns) =
        manifest::load_with_identity(&manifest::manifest_path(output_dir))?;
    // The two sidecars must be a matching pair: landing between an index's
    // manifest write and its meta write yields one half of each, and neither
    // half alone is a verdict (round 4).
    if !meta.describes_manifest(size, mtime_ns) {
        return None;
    }
    Some(CheckInputs {
        root,
        files: loaded.files,
        commit_sha: meta.commit_sha,
    })
}

/// The receipt itself, over inputs already known to be admissible.
fn verdict(inputs: &CheckInputs) -> Value {
    let dirty_files = count_dirty(&inputs.root, &inputs.files);
    let divergence = commit_divergence(&inputs.root, inputs.commit_sha.as_deref());
    // A HEAD that sits on a different commit than the one indexed is staleness
    // the file check cannot see: a commit that only ADDS files leaves every
    // manifested file byte-identical, and `count_dirty` has no entry to compare
    // an added file against. Either direction counts — moved forward, moved
    // back, or diverged onto another branch — so the verdict is a plain OR over
    // the two signals, not a special case per direction.
    let head_moved = divergence.is_some_and(|(ahead, behind)| ahead > 0 || behind > 0);
    // "fresh" has to mean something was checked and came back clean. An empty
    // manifest with no commit provenance examined nothing at all, and a clean
    // bill for an examination that never happened is the same error as trusting
    // an unverifiable manifest key (round 4). Decided here rather than in
    // `admissible_inputs` because it needs `divergence`, and asking git for it
    // twice to move the gate earlier would cost a subprocess for nothing.
    if inputs.files.is_empty() && divergence.is_none() {
        return unknown();
    }
    json!({
        "state": if dirty_files == 0 && !head_moved { "fresh" } else { "stale" },
        "checked_files": inputs.files.len(),
        "dirty_files": dirty_files,
        "commits_behind": divergence.map(|(_, behind)| behind),
        "commits_ahead": divergence.map(|(ahead, _)| ahead),
    })
}

/// The indexed root, accepted ONLY after the sidecar's claim survives policy.
///
/// `meta.json` is attacker-craftable, exactly like `commit_sha` and the manifest
/// keys this module already guards. `root` is the join base for every `stat` in
/// `count_dirty` AND the `-C` argument to `git`, so an unvalidated value walks
/// straight past the round-3 key-containment fix: `"root": "/"` with an
/// perfectly ordinary key like `"etc/shadow"` passes `is_contained_key` and
/// resolves to `/etc/shadow`. Same existence-and-attributes oracle, entered
/// through a different field of the same file (fleet-watch#112 review round 4).
///
/// The policy: absolute, resolvable, a directory, and not one of the system
/// roots `validate_graph_path_safe` already refuses to treat as a working path.
/// `canonicalize` is what makes the later joins meaningful — it resolves `..`
/// and any symlink IN THE ROOT ITSELF once per call, so the containment
/// `is_contained_key` enforces per key is enforced against a real directory
/// rather than a string.
///
/// RESIDUAL EXPOSURE — a bounded mitigation, NOT a closure. Stated precisely so
/// no reader mistakes this for authentication of the root:
///
/// > An attacker with write access to `output_dir` can still direct `root` at
/// > any real, absolute, resolvable, non-blacklisted directory on the host and
/// > use `dirty_files` / `state` as an existence, size and mtime oracle for
/// > files under it.
///
/// That is the same prerequisite this module's threat model already assumes for
/// the vectors it DOES close (`commit_sha`, manifest keys) — the difference is
/// that those are shut and this one is only narrowed: what no longer works is a
/// non-existent, non-canonicalizable, non-directory, or blacklisted-system-root
/// value.
///
/// Nothing on disk can close it, because anything written beside the sidecar is
/// equally forgeable. The two real closures are API/architecture decisions
/// deliberately NOT taken here: a caller-supplied `codebase_path` on all three
/// read tools is a breaking schema change for every MCP consumer, and a signed
/// sidecar breaks the portable-artifact use case of moving a built graph
/// between machines. Both are raised with the owner outside this change.
fn validated_root(claimed: &str) -> Option<PathBuf> {
    let claimed = Path::new(claimed);
    if !claimed.is_absolute() {
        return None;
    }
    let root = std::fs::canonicalize(claimed).ok()?;
    if !root.is_dir() {
        return None;
    }
    if forbidden_roots().contains(&root) {
        return None;
    }
    Some(root)
}

/// The system roots this check refuses, in BOTH their literal and canonical
/// forms, resolved once per process.
///
/// Comparing a canonicalized root against literal strings does not work, and
/// silently did not: on macOS `/etc`, `/tmp`, `/var` and `/home` are themselves
/// symlinks, so `"root": "/etc"` canonicalizes to `/private/etc` — a path the
/// literal list never mentions. The blacklist accepted it, `count_dirty` joined
/// attacker-chosen manifest keys onto it, and the oracle round 4 closed for
/// `"root": "/"` was open again through a real directory (fleet-watch#112
/// review round 5; verified on this host — `/etc` → `/private/etc`, `/tmp` →
/// `/private/tmp`, `/var` → `/private/var`, `/home` → `/System/Volumes/Data/home`).
///
/// Both forms are kept because neither subsumes the other: the canonical form
/// is what a canonicalized input will equal on macOS, and the literal form is
/// what it will equal on a platform where the path is a real directory. An
/// entry that does not resolve on this platform contributes only its literal.
fn forbidden_roots() -> &'static [PathBuf] {
    static ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut roots = Vec::new();
        for entry in crate::query_handlers::FORBIDDEN_GRAPH_PATH_PREFIXES {
            let literal = PathBuf::from(entry);
            if let Ok(canonical) = std::fs::canonicalize(&literal) {
                if canonical != literal {
                    roots.push(canonical);
                }
            }
            roots.push(literal);
        }
        roots
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
