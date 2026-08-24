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
/// Best-effort: a failed write is logged and ignored. The graph is the
/// product; the sidecar is a convenience for consumers, and its absence just
/// degrades a consumer's path reconstruction, never the index.
pub(crate) fn write_graph_meta(output_dir: &Path, root: &Path) {
    let meta = json!({
        "schema_version": 1,
        "root": root.to_string_lossy(),
        "tool": "ai-architect-mcp-codebase",
    });
    let meta_path = output_dir.join("meta.json");
    if let Err(e) = fs::write(&meta_path, meta.to_string()) {
        eprintln!("[ap] write graph meta {}: {e}", meta_path.display());
    }
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

        write_graph_meta(&base, &root);

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
            Some(1),
        );
        let _ = fs::remove_dir_all(&base);
    }
}
