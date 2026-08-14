// tests/common — shared integration-test-only tempdir guard.
//
// Mirrors `src/test_support.rs` (see that file's doc comment for the root
// cause and the fix). Duplicated rather than exposed from the library crate
// because `cfg(test)` is never set when integration tests in `tests/*.rs`
// link the library — Cargo builds the lib crate those tests depend on
// WITHOUT `--cfg test`, so a `#[cfg(test)]` item in the library is invisible
// here regardless of `pub`. `tests/common/mod.rs` is Rust's own idiom for
// sharing code between integration test binaries without it being picked up
// as an integration test target itself.
#![allow(dead_code)] // not every integration test file uses every item here.

use std::ops::Deref;
use std::path::Path;

pub struct TestTempDir(Option<tempfile::TempDir>);

impl TestTempDir {
    pub fn path(&self) -> &Path {
        self.0
            .as_ref()
            .expect("TestTempDir inner guard only taken in Drop")
            .path()
    }
}

impl Deref for TestTempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        self.path()
    }
}

impl AsRef<Path> for TestTempDir {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            if std::thread::panicking() {
                let _ = dir.keep();
                return;
            }
            // Mirror of src/test_support.rs: a DB fixture path re-created as
            // a FILE (LadybugDB single-file layout) defeats TempDir's
            // remove_dir_all, which fails silently in Drop and leaks ~368 MB
            // per fixture. Remove the file form explicitly.
            if dir.path().is_file() {
                let path = dir.keep();
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

pub trait TempDirExt {
    fn keep_managed(self) -> TestTempDir;
}

impl TempDirExt for tempfile::TempDir {
    fn keep_managed(self) -> TestTempDir {
        TestTempDir(Some(self))
    }
}
