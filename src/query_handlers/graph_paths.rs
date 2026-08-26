//! Filesystem safety for the caller-supplied `graph_path` / `output_dir`
//! arguments, and the sidecar written next to a graph.
//!
//! Split from `query_handlers` when that file crossed the §4.1 500-line cap.
//! Grouped by what it protects: these are the only functions in the module
//! that touch the filesystem on a caller-derived path.

use serde_json::json;
use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// Graph-path safety — source: H4 fix.
//
// The `graph_path` / `output_dir` / ... arguments are caller-controlled and
// in the pre-fix code were passed to `remove_dir_all`. A malicious caller
// could set `output_dir: "/"` and have the server wipe the filesystem.
//
// `validate_graph_path_safe` MUST be called before any `remove_dir_all` or
// `create_dir_all` on a caller-derived path. The policy:
//   (a) path must be absolute,
//   (b) last segment must be `graph` (or the path must contain `/graph/`),
//   (c) path must NOT equal a forbidden system root.
// ---------------------------------------------------------------------------

pub(crate) const FORBIDDEN_GRAPH_PATH_PREFIXES: &[&str] = &[
    "/", "/Users", "/home", "/root", "/tmp", "/var", "/etc", "/usr", "/bin", "/sbin", "/dev",
    "/opt", "/System", "/Library",
];

/// Returns Ok iff `path` is a safe target for destructive directory ops.
pub(crate) fn validate_graph_path_safe(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!(
            "unsafe_graph_path: must be absolute (got {:?})",
            path
        ));
    }
    // Must end in `/graph` (the well-known suffix). Check the last component.
    let last = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if last != "graph" {
        return Err(format!(
            "unsafe_graph_path: must end in '/graph' (got {:?})",
            path
        ));
    }
    // Reject pathological roots (even if they happen to end in `/graph`).
    let s = path.to_string_lossy();
    for forbidden in FORBIDDEN_GRAPH_PATH_PREFIXES {
        if s == *forbidden || s == format!("{forbidden}/graph") {
            return Err(format!(
                "unsafe_graph_path: {path:?} is a forbidden system path"
            ));
        }
    }
    Ok(())
}

/// Removes a stale graph artifact at `path`, whether the prior run left a
/// directory (older Kuzu lays the database out as a dir) or a single database
/// file (newer Kuzu). Plain `remove_dir_all` fails with `ENOTDIR (os error 20)`
/// when the target is a file — the observed failure on re-index of an existing
/// graph. `symlink_metadata` never traverses a symlink at the graph path, so a
/// symlinked `graph` is unlinked, not followed.
/// Caller MUST have run `validate_graph_path_safe` first.
pub(crate) fn remove_stale_graph_artifact(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("stat stale graph path: {e}"))?;
    let outcome = if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    outcome.map_err(|e| format!("remove stale graph artifact: {e}"))
}

/// Write a ``meta.json`` sidecar in ``output_dir`` recording the ABSOLUTE
/// source root this graph was indexed from.
///
/// AP stores file paths RELATIVE to the indexed root so the graph stays
/// portable across machines. A downstream consumer that must reconstruct
/// absolute paths — cortex-viz keys its FILE nodes by the absolute path (tool
/// events + wiki-page -> source-file joins) — needs that root, which is
/// otherwise consumed at index time and discarded. Persisting it in a sidecar
/// (not inside the graph) keeps the graph file itself free of machine-specific
/// paths: the structure stays portable, and the machine-specific root lives in
/// a file that is naturally regenerated on the next re-index.
///
/// Schema 2 (fleet-watch#112) adds `commit_sha`: the indexed root's git HEAD
/// at write time, `None` outside a git working tree. `graph_freshness` reads
/// it back to count how far the root's CURRENT HEAD has moved from it in
/// EITHER direction, using the same `git_head` used to compute it here — a
/// schema-1
/// sidecar (no `commit_sha` key) simply parses with the field absent, so an
/// old sidecar degrades to the dirty-file signal alone rather than failing.
///
/// Atomic, via the shared `handler_util::atomic_write` (POSIX rename(2), IEEE
/// Std 1003.1-2017). Required as of fleet-watch#112: a plain `fs::write`
/// truncates the file in place, so until this PR made `graph_freshness::check`
/// the FIRST query-time reader of `meta.json`, the window between truncate and
/// write had no reader to expose it. It does now — a re-index running while a
/// read tool is answering could hand that reader an empty or half-written
/// sidecar, which parses as "no provenance" and silently downgrades the
/// staleness verdict.
///
/// The shared helper rather than a local temp+rename (fleet-watch#112 review
/// round 5): an earlier revision here hand-rolled the same shape and, doing so,
/// dropped the `sync_all` the helper already performs — so a crash between the
/// write and the rename could publish an empty or truncated sidecar, a
/// durability regression against a guarantee this repository already provided
/// one module away. The helper also derives a per-writer temp name from pid,
/// clock and a random suffix, which is what keeps two `index_codebase` calls
/// aimed at one `output_dir` from interleaving through a shared temp path.
///
/// The sibling `indexer::manifest::save` writes the other sidecar into this
/// same directory with the same temp-file-plus-rename SHAPE, but not through
/// this helper: its temp path is a fixed `file_manifest.json.tmp` and it does
/// not fsync, so it still carries both exposures. Stated rather than glossed as
/// parity, because an earlier revision of this comment claimed the two matched
/// and they do not. Routing it through `atomic_write` too is agreed follow-up
/// work, deliberately not done in this change.
///
/// Returns the failure instead of swallowing it (fleet-watch#112 review round
/// 5). This used to `eprintln!` and carry on, which was defensible while the
/// sidecar was only a convenience for path reconstruction — it is not, now that
/// the staleness receipt reads it. On Windows a rename over a destination
/// another thread or process holds open can fail with a sharing violation, and
/// a swallowed failure there leaves the PREVIOUS `meta.json` in place while the
/// caller is told a fresh index completed. The regression test for that shape
/// runs only on Unix in CI (`windows-build.yml` builds but does not test), so
/// the guarantee cannot rest on the test — it has to rest on the caller being
/// told. Callers decide: every one of them logs loudly, and the ones with a
/// response attach the error to it. On failure the previous sidecar is left
/// intact rather than destroyed.
pub(crate) fn write_graph_meta(output_dir: &Path, root: &Path) -> Result<(), String> {
    // Schema 3 (fleet-watch#112 review round 4): record WHICH `file_manifest.json`
    // this sidecar accompanies. Both indexing paths write the manifest first and
    // this file last, so `meta.json` is an index's commit point; naming the
    // manifest lets a reader that lands mid-index see that the pair does not
    // match and report "unknown" instead of pairing a fresh commit sha with a
    // stale manifest and calling a just-rebuilt graph stale. Absent manifest →
    // nulls, and the reader has no manifest to check either.
    let manifest = fs::metadata(crate::indexer::manifest::manifest_path(output_dir)).ok();
    let meta = json!({
        "schema_version": 3,
        "root": root.to_string_lossy(),
        "tool": "ai-architect-mcp-codebase",
        "commit_sha": crate::git_provenance::git_head(root),
        "manifest_size": manifest.as_ref().map(|m| m.len()),
        "manifest_mtime_ns": manifest.as_ref().map(crate::indexer::manifest::mtime_ns),
    });
    crate::handler_util::atomic_write(&output_dir.join("meta.json"), meta.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graph_path_must_end_in_graph() {
        // source: H4 fix — caller-chosen path is safe ONLY when it is absolute
        // AND the last segment is exactly `graph` AND the path is not one of
        // the forbidden system roots.
        assert!(validate_graph_path_safe(Path::new("/tmp/foo/graph")).is_ok());
        assert!(validate_graph_path_safe(Path::new("/Users/alice/proj/graph")).is_ok());

        // Not absolute.
        assert!(validate_graph_path_safe(Path::new("relative/graph")).is_err());

        // Does not end in /graph.
        assert!(validate_graph_path_safe(Path::new("/etc")).is_err());
        assert!(validate_graph_path_safe(Path::new("/tmp")).is_err());
        assert!(validate_graph_path_safe(Path::new("/")).is_err());
        assert!(validate_graph_path_safe(Path::new("/Users")).is_err());
        assert!(validate_graph_path_safe(Path::new("/tmp/foo/notgraph")).is_err());

        // Ends in /graph but IS a forbidden system root (should still reject).
        assert!(validate_graph_path_safe(Path::new("/etc/graph")).is_err());
        assert!(
            validate_graph_path_safe(Path::new("//graph")).is_err()
                || validate_graph_path_safe(Path::new("//graph")).is_ok()
        );
    }

    #[test]
    fn remove_stale_graph_artifact_handles_file_and_dir() {
        // source: ENOTDIR fix — a prior run can leave `graph` as a single-file
        // Kuzu db; `remove_dir_all` on a file returns ENOTDIR (os error 20).
        // The helper must delete both shapes and report a missing path as an
        // error rather than panicking.
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        use crate::test_support::TempDirExt;
        let base = tempfile::Builder::new()
            .prefix("ap-remove-stale-")
            .tempdir()
            .expect("create temp dir")
            .keep_managed();
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let graph = base.join("graph");

        // Case A: graph is a directory with nested content.
        fs::create_dir_all(graph.join("nested")).unwrap();
        fs::write(graph.join("nested/f.txt"), b"x").unwrap();
        assert!(graph.is_dir());
        remove_stale_graph_artifact(&graph).expect("dir removal");
        assert!(!graph.exists());

        // Case B: graph is a single file — the ENOTDIR regression case.
        fs::write(&graph, b"kuzu-single-file-db").unwrap();
        assert!(graph.is_file());
        remove_stale_graph_artifact(&graph).expect("file removal (was ENOTDIR)");
        assert!(!graph.exists());

        // Missing path → surfaced as an error, never a panic.
        assert!(remove_stale_graph_artifact(&graph).is_err());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn write_graph_meta_records_absolute_root() {
        // The sidecar records the ABSOLUTE indexed root so a consumer can
        // rebuild absolute paths from AP's relative ones (cortex-viz wiki->file
        // join + tool-file keying). It is written NEXT TO the graph, never
        // inside it — the graph itself stays portable.
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        use crate::test_support::TempDirExt;
        let base = tempfile::Builder::new()
            .prefix("ap-meta-")
            .tempdir()
            .expect("create temp dir")
            .keep_managed();
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let root = base.join("some/repo/root");

        write_graph_meta(&base, &root).expect("write meta");

        let meta_path = base.join("meta.json");
        assert!(meta_path.is_file(), "meta.json must be written");
        let parsed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();
        assert_eq!(
            parsed.get("root").and_then(|v| v.as_str()),
            Some(root.to_string_lossy().as_ref()),
            "sidecar must record the absolute root verbatim",
        );
        assert_eq!(
            parsed.get("schema_version").and_then(|v| v.as_u64()),
            Some(3),
        );
        // `root` is not a git working tree → commit_sha is present but null,
        // not simply absent (the field is always written, per schema 3).
        assert!(parsed.get("commit_sha").is_some_and(|v| v.is_null()));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn write_graph_meta_replaces_the_sidecar_atomically() {
        // fleet-watch#112 review: this PR made `graph_freshness::check` the
        // first query-time READER of meta.json, so a re-index writing it while
        // a read tool answers can now hand that reader a torn file. A plain
        // `fs::write` truncates and rewrites the SAME inode, so a reader that
        // already opened the sidecar watches the content change underneath it —
        // asserted here without any wall-clock: the handle is opened before the
        // second write and read after it. tmp+rename swaps the directory entry
        // instead, so the open reader keeps the whole previous version.
        use crate::test_support::TempDirExt;
        use std::io::Read;
        let base = tempfile::Builder::new()
            .prefix("ap-meta-atomic-")
            .tempdir()
            .expect("create temp dir")
            .keep_managed();
        let first_root = base.join("repo/one");
        let second_root = base.join("repo/two-with-a-much-longer-name");

        write_graph_meta(&base, &first_root).expect("write meta");
        let mut held = fs::File::open(base.join("meta.json")).expect("open sidecar");

        // A concurrent re-index rewrites the sidecar while `held` is open.
        write_graph_meta(&base, &second_root).expect("write meta");

        let mut seen = String::new();
        held.read_to_string(&mut seen).expect("read held handle");
        let parsed: serde_json::Value =
            serde_json::from_str(&seen).expect("an open reader must never see a torn sidecar");
        assert_eq!(
            parsed.get("root").and_then(|v| v.as_str()),
            Some(first_root.to_string_lossy().as_ref()),
            "the rewrite must land on a new inode, leaving the open reader's view whole",
        );

        // The replacement is visible to a reader that opens after the rename,
        // and no temp file survives.
        let now: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(base.join("meta.json")).unwrap()).unwrap();
        assert_eq!(
            now.get("root").and_then(|v| v.as_str()),
            Some(second_root.to_string_lossy().as_ref()),
        );
        assert!(
            !leftover_tmp_files(&base),
            "no temp file may survive a successful write",
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// True when any temp artifact remains directly under `dir`. Matches the
    /// shape `handler_util::atomic_write` uses: `.<name>.tmp.<pid>.<s>.<ns>.<rand>`.
    fn leftover_tmp_files(dir: &Path) -> bool {
        fs::read_dir(dir)
            .expect("read output dir")
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".tmp."))
    }

    #[test]
    fn repeated_writes_leave_one_sidecar_and_no_temp_residue() {
        // The temp-name uniqueness that used to live here now belongs to
        // `handler_util::atomic_write`, which derives it from pid, clock and a
        // random suffix. What this module still owns is the OUTCOME: repeated
        // writes into one `output_dir` converge on a single, complete
        // `meta.json` and leave nothing behind — the observable that a shared
        // temp path would break (fleet-watch#112 review round 5).
        use crate::test_support::TempDirExt;
        let base = tempfile::Builder::new()
            .prefix("ap-meta-repeat-")
            .tempdir()
            .expect("create temp dir")
            .keep_managed();
        let first_root = base.join("repo/one");
        let second_root = base.join("repo/two");

        write_graph_meta(&base, &first_root).expect("first write");
        write_graph_meta(&base, &second_root).expect("second write");

        let parsed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(base.join("meta.json")).unwrap())
                .expect("the surviving sidecar must be complete JSON");
        assert_eq!(
            parsed.get("root").and_then(|v| v.as_str()),
            Some(second_root.to_string_lossy().as_ref()),
        );
        assert!(!leftover_tmp_files(&base), "no temp artifact may survive");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_failed_sidecar_write_is_reported_not_swallowed() {
        // fleet-watch#112 review round 5, finding 3. This used to `eprintln!`
        // and return `()`, so a caller could not tell a written sidecar from an
        // unwritten one — on Windows a rename over a handle another process
        // holds open fails with a sharing violation, and the swallowed failure
        // leaves the PREVIOUS meta.json in place while the caller is told a
        // fresh index completed. The old signature made that unobservable by
        // construction; this asserts it is observable now.
        use crate::test_support::TempDirExt;
        let base = tempfile::Builder::new()
            .prefix("ap-meta-fail-")
            .tempdir()
            .expect("create temp dir")
            .keep_managed();
        // A regular FILE standing where the output directory must be, so the
        // write cannot succeed no matter the platform.
        let blocked = base.join("occupied-by-a-file");
        fs::write(&blocked, b"not a directory").expect("write blocker");

        let err = write_graph_meta(&blocked, &base)
            .expect_err("a sidecar that cannot be written must say so");
        assert!(
            err.contains("atomic_write"),
            "the error must name the failing operation: {err}",
        );
        let _ = fs::remove_dir_all(&base);
    }
}
