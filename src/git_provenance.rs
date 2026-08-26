//! Git provenance primitives — the three `git` shell-outs this crate uses to
//! ask "what commit is this tree on, and how far is it from that one?".
//!
//! Extracted from `artifact.rs` (fleet-watch#112 review): that module is the
//! artifact export/import adapter and was already past the §4.1 500-line cap
//! before this work; exposing two of its private helpers to `graph_freshness`
//! would have grown it further instead of putting a shared concern where it
//! belongs. The concern is cohesive on its own — provenance READS, no writes,
//! no artifact knowledge — and is now reachable directly rather than only
//! through an unrelated module.
//!
//! NOT yet the only copy, despite what an earlier revision of this comment
//! claimed (fleet-watch#112 review round 6). `indexer::incremental` still
//! carries its own `is_hex_sha`, and its comment points at `artifact::is_hex_sha`
//! — a path that no longer exists since the function moved here. `history` and
//! `cochange` each carry their own `git_head`. Consolidating those is real work
//! with its own blast radius, tracked as follow-up; asserting it was already
//! done was worse than leaving it undone.
//!
//! Layer: infrastructure/adapter. Depends on std only. Every call passes its
//! arguments to `Command` directly, never through a shell, so there is no
//! interpolation surface; the remaining hazard is ARG injection, which
//! `is_hex_sha` guards at the two call sites that read a sha off disk.

use std::path::Path;
use std::process::Command;

/// Returns the git HEAD sha for `repo_path`, or `None` if it is not a git
/// working tree. Uses `Command` args (no shell) — injection-safe, matching the
/// pattern in `history/mod.rs`.
///
/// Two callers: the index-time provenance stamp `query_handlers::write_graph_meta`
/// records into `meta.json`, and the query-time current-HEAD read in
/// `graph_freshness` that compares against it (fleet-watch#112).
pub(crate) fn git_head(repo_path: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

/// `git rev-list --count <from>..<to>`. `None` if the command fails (e.g. the
/// `from` sha is unknown to the repo) or the output does not parse.
///
/// One-directional: it answers "how many commits does `to` have that `from`
/// does not", and returns 0 when `to` is an ANCESTOR of `from`. A caller that
/// cannot assume the ordering needs both counts — see
/// `graph_freshness::commit_divergence`, which asks git for the symmetric
/// difference in a single call instead.
pub(crate) fn commits_between(repo_path: &Path, from: &str, to: &str) -> Option<u64> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-list", "--count"])
        .arg(format!("{from}..{to}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .ok()
}

/// True for a plausible git object sha (non-empty, all ASCII hex).
///
/// Guards every sha this crate reads off disk before it reaches a `git`
/// argument: the artifact sidecar's `commit` field (`artifact::artifact_staleness`)
/// and the graph sidecar's `commit_sha` (`graph_freshness`). Both files are
/// attacker-craftable, and an unguarded value is not merely passed through —
/// git EVALUATES it, so `HEAD~1` yields a real count for provenance the sidecar
/// never held, and a `-`-leading value smuggles a flag into `git rev-list`
/// (arg injection, not shell injection).
pub(crate) fn is_hex_sha(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_hex_sha_guards_arg_injection() {
        assert!(is_hex_sha("0a1b2c3d4e5f"));
        assert!(!is_hex_sha("")); // absent provenance
        assert!(!is_hex_sha("--all")); // git flag smuggling attempt
        assert!(!is_hex_sha("HEAD~1")); // non-hex revision expression
    }

    #[test]
    fn git_head_is_none_outside_a_working_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(git_head(tmp.path()).is_none());
    }
}
