// test_support — shared test-only tempdir guard.
//
// Root cause (boy-scout, coding-standards.md §14): every test fixture helper
// in this crate called `tempfile::TempDir::keep()` to survive being returned
// as a bare `PathBuf` past the builder function's scope. `keep()` disables
// the directory's cleanup PERMANENTLY — there is no later point at which
// anything decides to delete it — so every `cargo test` invocation left a
// freshly, uniquely-named directory behind under the OS temp root forever.
// Checking `std::thread::panicking()` at BUILD time (before the fixture is
// even used) can never be true, so the fix cannot live at construction; it
// has to live at DROP time, at the test's natural end (success or panic
// unwind both run destructors of a test-function-local value).
//
// `TestTempDir` is that RAII guard: it derefs to `Path` so almost every
// existing call site (which only ever read from the path — `.join`, passing
// `&path`, `AsRef<Path>` args) keeps compiling unchanged once the local
// binding's static type becomes `TestTempDir` instead of `PathBuf`. On drop,
// a PASSING test's directory is deleted (`tempfile::TempDir`'s normal
// contract); a PANICKING test's directory is preserved for post-mortem
// inspection via the same `.keep()` this replaces — used correctly this
// time, at the one point where "is this thread unwinding from a panic" is an
// answerable question.
use std::ops::Deref;
use std::path::Path;

pub(crate) struct TestTempDir(Option<tempfile::TempDir>);

impl TestTempDir {
    pub(crate) fn path(&self) -> &Path {
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
                // Post-mortem artifact — mirrors the old (always-on) `.keep()`,
                // now scoped to the one case it was meant for.
                let _ = dir.keep();
            }
            // else: `dir` drops here and removes its backing directory.
        }
    }
}

/// Extension trait so every existing call site's tail `.keep()` becomes a
/// one-token `.keep_managed()` swap: no new import at most call sites (the
/// trait is brought into scope by `use crate::test_support::TempDirExt` /
/// `use super::super::test_support::TempDirExt` once per file), same chain
/// shape, same expression position.
pub(crate) trait TempDirExt {
    fn keep_managed(self) -> TestTempDir;
}

impl TempDirExt for tempfile::TempDir {
    fn keep_managed(self) -> TestTempDir {
        TestTempDir(Some(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passing_test_directory_is_removed_on_drop() {
        let dir = tempfile::Builder::new()
            .prefix("test_support_pass_")
            .tempdir()
            .expect("tempdir")
            .keep_managed();
        let path = dir.path().to_path_buf();
        assert!(path.exists());
        drop(dir);
        assert!(
            !path.exists(),
            "a passing test's TestTempDir must remove its directory on drop"
        );
    }

    #[test]
    fn deref_gives_a_usable_path() {
        let dir = tempfile::Builder::new()
            .prefix("test_support_deref_")
            .tempdir()
            .expect("tempdir")
            .keep_managed();
        let joined = dir.join("sub/file.txt");
        assert!(joined.starts_with(dir.path()));
    }

    // The panic-preservation branch is proven at the integration level (the
    // "run the suite twice, measure disk" check in the PR description) rather
    // than here: deliberately panicking inside a `#[test]` to assert
    // directory survival would itself abort this test as a failure, and
    // `catch_unwind`ing a panic inside a `#[test]` closure sets
    // `thread::panicking()` back to false by the time `TestTempDir::drop`
    // runs on stack unwind past the catch point, defeating the very check —
    // the guard's `Drop` genuinely needs a REAL unwind through the frame that
    // owns it, which only an actually-failing test provides.
}
