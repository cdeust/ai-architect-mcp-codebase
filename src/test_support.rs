//! Shared `#[cfg(test)]` fixture-cleanup guard.
//!
//! `tempfile::TempDir::keep()` is used throughout this suite (see the
//! "issue #25 audit" comments in `indexer/tests.rs`) so a *failing* test's
//! on-disk database survives for inspection instead of being deleted by
//! `tempfile`'s own `Drop`. `.keep()` hands ownership of the directory to
//! the caller, which means every caller must delete it on success or the
//! fixture leaks permanently — two fixtures in this workspace
//! (`prd_handlers_tests::build_fixture`/`tmp_dir`,
//! `verification_ops_tests::tmp_out`) never did. That was invisible while
//! the persisted graph was semantic-only (~1 MB); it stops being invisible
//! once a fixture persists the full AST (~300 MB measured — see the
//! full-AST-persistence PR description), where a handful of missed
//! call sites is enough to exhaust a small disk across one `cargo test`
//! run. `TempGraphDir` makes "clean up on drop" the default instead of a
//! per-call-site obligation, without weakening the debuggability the
//! `.keep()` pattern exists for.
//!
//! precondition: `root` names a directory this process owns exclusively
//! (freshly created, or freshly reset via `remove_dir_all` immediately
//! before use — the existing `.keep()` call sites already do this before
//! constructing a guard).
//! postcondition: when the guard drops while the current thread is NOT
//! unwinding from a panic (the test passed), `root` and everything under
//! it — including `exposed` when it differs from `root` — is removed.
//! When the guard drops while `std::thread::panicking()` is true (an
//! assertion upstream in the same test failed), the directory is left in
//! place: `.keep()`'s "inspect the fixture after a failure" contract is
//! preserved.
//! invariant: the guard is the sole owner of `root`'s removal; no other
//! code in a fixture using this guard calls `remove_dir_all` on `root`
//! after construction.
use std::ops::Deref;
use std::path::{Path, PathBuf};

pub(crate) struct TempGraphDir {
    /// The directory actually removed on drop.
    root: PathBuf,
    /// The path callers dereference — a subdirectory of `root` when a
    /// fixture builds more than the graph under one temp root (e.g. a
    /// source tree alongside the graph it was indexed into), otherwise
    /// identical to `root`.
    exposed: PathBuf,
}

impl TempGraphDir {
    /// `root` and the path callers should use are the same directory.
    pub(crate) fn whole(root: PathBuf) -> Self {
        let exposed = root.clone();
        Self { root, exposed }
    }

    /// `exposed` is a subdirectory of `root`; dropping the guard removes
    /// `root` (and therefore everything built alongside `exposed`).
    pub(crate) fn subdir(root: PathBuf, exposed: PathBuf) -> Self {
        Self { root, exposed }
    }
}

impl Deref for TempGraphDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.exposed
    }
}

impl Drop for TempGraphDir {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
