// write_diagnostics.rs — turn a failed write/open at the storage boundary
// into an actionable, path-naming diagnostic (§13.1-F1).
//
// The MCP graph store and the index writer both hit ENOSPC on a full disk.
// lbug surfaces that as an opaque `unordered_map::at: key not found` abort and
// std's writers as a bare `io::Error`; neither mentions the disk. An agent
// then chases a phantom logic bug (issue #143). This module classifies the
// real condition — by probing whether the target's directory can still accept
// a write — and appends a message that names the path and the space/permission
// state, so an ENOSPC is never reported as a missing key.

use std::io;
use std::path::Path;

// ENOSPC — "No space left on device". POSIX value 28 on both Linux
// (asm-generic/errno-base.h) and macOS/Darwin (bsd/sys/errno.h).
// std's `io::ErrorKind::StorageFull` is not stable across our toolchain, so we
// match the raw errno directly.
// source: POSIX.1-2017 <errno.h>; Apple XNU bsd/sys/errno.h (ENOSPC == 28);
//         Linux include/uapi/asm-generic/errno-base.h (ENOSPC == 28).
const ENOSPC: i32 = 28;

/// True iff this `io::Error` is "no space left on device" (ENOSPC).
pub fn is_out_of_space(err: &io::Error) -> bool {
    err.raw_os_error() == Some(ENOSPC)
}

/// Probe whether `dir` can accept a write right now by creating and removing a
/// tiny sentinel file. Returns the `io::Error` verbatim when the write cannot
/// be made — this is the seam the tests inject through: a full disk yields
/// ENOSPC, an unwritable path yields `PermissionDenied`, a missing parent
/// yields `NotFound`. The probe is best-effort and side-effect-free on
/// success (the sentinel is removed).
pub fn probe_writable(dir: &Path) -> Result<(), io::Error> {
    let sentinel = dir.join(".ai-architect-write-probe");
    std::fs::write(&sentinel, b"")?;
    let _ = std::fs::remove_file(&sentinel);
    Ok(())
}

/// Annotate a failed write/open at `target` with the real, path-naming cause.
///
/// `underlying` is the low-level error text (an `io::Error` display, or lbug's
/// opaque abort string). We probe the target's parent directory to classify
/// what actually went wrong:
///   - probe succeeds → the failure was not a storage/permission condition; the
///     underlying message is returned unchanged (existing error-substring
///     contracts still hold).
///   - probe → ENOSPC → the disk is full; say so, name the directory.
///   - probe → other → name the directory and the concrete OS error kind.
pub fn annotate_write_failure(target: &Path, underlying: &str) -> String {
    let probe_dir = target.parent().unwrap_or(target);
    classify_write_failure(probe_dir, probe_writable(probe_dir), underlying)
}

/// Pure classifier over a probe outcome — the injectable seam. Separated from
/// `annotate_write_failure` so the ENOSPC branch can be exercised with a
/// synthetic `io::Error` (a real ENOSPC needs a full disk, which is not
/// portably reproducible in a unit test).
fn classify_write_failure(
    probe_dir: &Path,
    probe: Result<(), io::Error>,
    underlying: &str,
) -> String {
    match probe {
        Ok(()) => underlying.to_string(),
        Err(e) if is_out_of_space(&e) => format!(
            "{underlying} [no space left on device: a write probe to {} returned ENOSPC \
             (errno 28); free disk space and retry]",
            probe_dir.display()
        ),
        Err(e) => format!(
            "{underlying} [write probe to {} failed: {} ({})]",
            probe_dir.display(),
            e.kind(),
            e
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enospc_errno_is_detected() {
        // ENOSPC (28) → out of space; EACCES (13) and a synthetic kind → not.
        assert!(is_out_of_space(&io::Error::from_raw_os_error(ENOSPC)));
        assert!(!is_out_of_space(&io::Error::from_raw_os_error(13)));
        assert!(!is_out_of_space(&io::Error::other("not an os error")));
    }

    #[test]
    fn probe_writable_succeeds_on_a_writable_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(probe_writable(tmp.path()).is_ok());
        // Sentinel is cleaned up.
        assert!(!tmp.path().join(".ai-architect-write-probe").exists());
    }

    #[test]
    fn probe_writable_reports_missing_parent() {
        let missing = Path::new("/no-such-root-ai-architect-143/inner");
        let err = probe_writable(missing).expect_err("write to missing dir must fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn classify_enospc_probe_reports_no_space_and_names_dir() {
        // Synthetic ENOSPC drives the disk-full branch deterministically.
        let msg = classify_write_failure(
            Path::new("/data/graph"),
            Err(io::Error::from_raw_os_error(ENOSPC)),
            "lbug database open failed: unordered_map::at",
        );
        assert!(msg.contains("no space left on device"), "got: {msg}");
        assert!(msg.contains("/data/graph"), "names the dir, got: {msg}");
        assert!(
            msg.contains("unordered_map::at"),
            "underlying preserved: {msg}"
        );
    }

    #[test]
    fn classify_non_enospc_probe_error_names_dir_and_kind() {
        // A permission error must NOT be reported as out-of-space.
        let msg = classify_write_failure(
            Path::new("/data/graph"),
            Err(io::Error::from_raw_os_error(13)), // EACCES
            "lbug database open failed: x",
        );
        assert!(!msg.contains("no space left"), "not ENOSPC, got: {msg}");
        assert!(
            msg.contains("write probe") && msg.contains("failed"),
            "names the probe condition, got: {msg}"
        );
        assert!(msg.contains("/data/graph"), "names the dir, got: {msg}");
    }

    #[test]
    fn classify_ok_probe_returns_underlying_unchanged() {
        let msg = classify_write_failure(Path::new("/data/graph"), Ok(()), "some other failure");
        assert_eq!(msg, "some other failure");
    }

    #[test]
    fn annotate_leaves_non_storage_failures_unchanged() {
        // Parent is writable → the failure was something else; message is verbatim.
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("db");
        let msg = annotate_write_failure(&target, "lbug database open failed: some logic bug");
        assert_eq!(msg, "lbug database open failed: some logic bug");
    }

    #[test]
    fn annotate_names_the_path_when_the_target_is_unwritable() {
        // Point at a target whose parent cannot be written (does not exist):
        // the probe fails and the annotation names the directory + OS error.
        let target = Path::new("/no-such-root-ai-architect-143/inner/db");
        let msg = annotate_write_failure(target, "lbug database open failed: unordered_map::at");
        assert!(
            msg.contains("/no-such-root-ai-architect-143/inner"),
            "message must name the unwritable directory, got: {msg}"
        );
        assert!(
            msg.contains("write probe") && msg.contains("failed"),
            "message must name the OS-level write condition, got: {msg}"
        );
        // The underlying opaque text is preserved for continuity.
        assert!(
            msg.contains("unordered_map::at"),
            "underlying preserved: {msg}"
        );
    }
}
