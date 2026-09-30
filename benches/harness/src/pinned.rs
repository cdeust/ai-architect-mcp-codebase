// pinned.rs — materialise a corpus source tree from a pinned git revision.
//
// Why this exists (issue #397): the `rust-self` corpus used to index the live
// `src/` of this very repository, while its ground truth lists exhaustive
// per-file symbol sets and per-symbol caller sets. Every PR that added a
// function, an import, or a test therefore made a label wrong, and nothing
// failed: the bench is not a CI job, and the stale-path guard (#132) only
// notices a deleted *file*. The score decayed from 0.906 to 0.752 over ~30
// merged PRs without a single regression in the tools it measures.
// Pinning the corpus to a commit makes the labels describe an immutable tree,
// so a score change now means the tools changed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

/// A pinned source tree extracted into a private temporary directory. The
/// directory is removed when the value is dropped, so it must outlive every
/// use of `source_dir`.
#[derive(Debug)]
pub struct PinnedTree {
    _dir: TempDir,
    source_dir: PathBuf,
}

impl PinnedTree {
    /// Absolute path of the extracted subtree (what the indexer is pointed at).
    pub fn source_dir(&self) -> &Path {
        &self.source_dir
    }
}

/// Extract `<rev>:<subdir>` of the git repository containing `repo_hint`.
///
/// precondition:  `repo_hint` is a directory inside a git work tree (a bare
///                repo is rejected by `git_toplevel`); `rev` names a commit (or tree-ish) present in that
///                repo's object database; `subdir` is a repo-root-relative
///                directory of that tree.
/// postcondition: on `Ok`, `source_dir()` holds exactly the files of
///                `<rev>:<subdir>`, unaffected by the working tree, the index
///                or any later commit; on `Err`, nothing is left behind.
pub fn materialize(repo_hint: &Path, rev: &str, subdir: &str) -> Result<PinnedTree, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("tempdir for pinned tree: {e}"))?;
    let source_dir = dir.path().join("tree");
    std::fs::create_dir_all(&source_dir).map_err(|e| format!("create {source_dir:?}: {e}"))?;

    // `git archive` limits its output to the current directory's prefix, so a
    // hint that is a subdirectory of the repo (the corpus dir is) would yield
    // an empty archive. Run it from the work-tree root instead.
    let top = git_toplevel(repo_hint)?;
    // `--` in `extract_archive` keeps a `rev` starting with `-` from being read
    // as an option.
    let treeish = format!("{rev}:{subdir}");
    extract_archive(&top, &treeish, &source_dir)?;
    let extracted = std::fs::read_dir(&source_dir)
        .map_err(|e| format!("read {source_dir:?}: {e}"))?
        .next()
        .is_some();
    if !extracted {
        return Err(format!("pinned corpus {treeish:?} extracted no files"));
    }
    Ok(PinnedTree {
        _dir: dir,
        source_dir,
    })
}

/// Stream `git archive <treeish>` (run from `top`) into `tar -x -C dest`.
///
/// precondition:  `top` is a git work-tree root; `dest` is an existing directory.
/// postcondition: on `Ok`, both processes exited 0 and `dest` holds the archive;
///                on `Err`, the message names which process failed and why.
fn extract_archive(top: &Path, treeish: &str, dest: &Path) -> Result<(), String> {
    let mut archive = Command::new("git")
        .arg("-C")
        .arg(top)
        .args(["archive", "--format=tar", "--", treeish])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn `git archive`: {e}"))?;
    let tar_stdin = archive.stdout.take().ok_or("git archive: no stdout")?;
    let untar = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(dest)
        .stdin(Stdio::from(tar_stdin))
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("spawn `tar`: {e}"))?;
    let archive_out = archive
        .wait_with_output()
        .map_err(|e| format!("wait `git archive`: {e}"))?;
    if !archive_out.status.success() {
        return Err(format!(
            "pinned corpus rev {treeish:?} is not available ({}). A shallow clone drops \
             it: fetch full history (`git fetch --unshallow`).",
            String::from_utf8_lossy(&archive_out.stderr).trim()
        ));
    }
    if !untar.status.success() {
        return Err(format!(
            "tar failed extracting {treeish:?}: {}",
            String::from_utf8_lossy(&untar.stderr).trim()
        ));
    }
    Ok(())
}

/// Root of the git work tree holding `hint`. `git rev-parse --show-toplevel`
/// fails ("this operation must be run in a work tree") in a bare repository,
/// so a bare repo is reported as an error here, like any non-repository.
fn git_toplevel(hint: &Path) -> Result<PathBuf, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(hint)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("spawn `git rev-parse`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{hint:?} is not inside a git work tree: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// True iff `repo_hint`'s repository is a shallow clone that does not hold
/// `rev` in its object database. Distinguishes a CI checkout that
/// intentionally dropped history (`fetch-depth: 1`) — not a defect, the
/// caller should skip — from a full clone missing the same rev, which is a
/// real, hard-failing defect (issue #428: the CI floor job runs a shallow
/// checkout the `rust-self` corpus's pin cannot survive).
///
/// precondition:  `repo_hint` is a directory inside a git work tree.
/// postcondition: `Ok(true)` only when the repo is shallow AND `rev` is
///                absent from its object database; `Ok(false)` for a full
///                clone regardless of whether `rev` is present (a full
///                clone missing `rev` must still fail downstream, in
///                `materialize`, not be silently skipped here).
#[cfg(test)]
pub fn is_shallow_clone_missing_rev(repo_hint: &Path, rev: &str) -> Result<bool, String> {
    let top = git_toplevel(repo_hint)?;
    let shallow_out = Command::new("git")
        .arg("-C")
        .arg(&top)
        .args(["rev-parse", "--is-shallow-repository"])
        .output()
        .map_err(|e| format!("spawn `git rev-parse --is-shallow-repository`: {e}"))?;
    if !shallow_out.status.success() {
        return Err(format!(
            "git rev-parse --is-shallow-repository: {}",
            String::from_utf8_lossy(&shallow_out.stderr).trim()
        ));
    }
    let is_shallow = String::from_utf8_lossy(&shallow_out.stdout).trim() == "true";
    if !is_shallow {
        return Ok(false);
    }
    let cat_out = Command::new("git")
        .arg("-C")
        .arg(&top)
        .args(["cat-file", "-e", &format!("{rev}^{{commit}}")])
        .output()
        .map_err(|e| format!("spawn `git cat-file -e`: {e}"))?;
    Ok(!cat_out.status.success())
}

/// First `(corpus dir, name, rev)` in `pins` whose rev is absent only
/// because of a shallow checkout — see `is_shallow_clone_missing_rev` for
/// the shallow-vs-full-clone distinction that keeps a full clone genuinely
/// missing the rev a hard failure downstream, in `materialize` (issue #428).
#[cfg(test)]
pub fn shallow_skip_reason(pins: &[(PathBuf, String, String)]) -> Option<String> {
    for (dir, name, rev) in pins {
        if is_shallow_clone_missing_rev(dir, rev).unwrap_or(false) {
            return Some(format!(
                "corpus {name}: pinned rev {rev} is absent from this shallow checkout \
                 (fetch-depth drops history); not a corpus defect"
            ));
        }
    }
    None
}

/// Writes straight to fd 2, bypassing libtest's output capture (which
/// intercepts `eprintln!` on a passing test too) so a skip reason stays
/// visible in CI without a `.github/`-workflow `--nocapture` flag this
/// repo's owner has not approved (issue #428).
#[cfg(test)]
pub fn eprint_uncaptured(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/stderr") {
        let _ = writeln!(f, "{msg}");
    } else {
        eprintln!("{msg}");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit_file(repo: &Path, content: &str) -> String {
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), content).unwrap();
        git(repo, &["add", "src/a.rs"]);
        git(repo, &["commit", "-q", "-m", "c"]);
        git(repo, &["rev-parse", "HEAD"])
    }

    #[test]
    fn extracts_the_pinned_revision_not_the_working_tree() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        let old = commit_file(tmp.path(), "fn old() {}");
        commit_file(tmp.path(), "fn newer() {}");
        // Uncommitted edit: must not leak into the pinned tree either.
        std::fs::write(tmp.path().join("src/a.rs"), "fn dirty() {}").unwrap();

        let tree = materialize(tmp.path(), &old, "src").expect("materialize");
        let got = std::fs::read_to_string(tree.source_dir().join("a.rs")).unwrap();
        assert_eq!(got, "fn old() {}");
    }

    #[test]
    fn a_hint_below_the_repo_root_still_yields_the_whole_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        let rev = commit_file(tmp.path(), "fn a() {}");
        let deep = tmp.path().join("benches/corpora/x");
        std::fs::create_dir_all(&deep).unwrap();
        let tree = materialize(&deep, &rev, "src").expect("materialize from a subdirectory");
        assert!(tree.source_dir().join("a.rs").exists());
    }

    #[test]
    fn a_rev_the_repo_does_not_have_is_a_named_error_not_an_empty_tree() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        commit_file(tmp.path(), "fn a() {}");
        let err = materialize(tmp.path(), &"0".repeat(40), "src").unwrap_err();
        assert!(err.contains("not available"), "{err}");
        assert!(err.contains("git fetch --unshallow"), "{err}");
    }

    #[test]
    fn an_empty_archive_is_a_named_error_not_an_empty_corpus() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        // The empty tree is built into git; its archive is valid but has no files.
        let empty_tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
        let err = materialize(tmp.path(), empty_tree, "").unwrap_err();
        assert!(err.contains("extracted no files"), "{err}");
    }

    #[test]
    fn a_hint_outside_any_git_work_tree_is_a_named_error() {
        // A fresh temp dir sits under the OS temp root, which is not inside a repo.
        let tmp = tempfile::tempdir().unwrap();
        let err = materialize(tmp.path(), "HEAD", "src").unwrap_err();
        assert!(err.contains("is not inside a git work tree"), "{err}");
    }

    #[test]
    fn a_rev_starting_with_a_dash_is_not_read_as_an_option() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        commit_file(tmp.path(), "fn a() {}");
        let err = materialize(tmp.path(), "--output=/dev/null", "src").unwrap_err();
        // With `--`, git reads it as a (missing) object name; without, as an option.
        assert!(err.contains("not a valid object name"), "{err}");
    }

    #[test]
    fn the_tree_is_removed_when_the_value_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        let rev = commit_file(tmp.path(), "fn a() {}");
        let tree = materialize(tmp.path(), &rev, "src").unwrap();
        let path = tree.source_dir().to_path_buf();
        assert!(path.exists());
        drop(tree);
        assert!(!path.exists());
    }

    /// A shallow clone of a *disposable* repo this test creates — never the
    /// host repo (issue #428's mechanics tests stay isolated from the
    /// checkout running them). `git clone --depth 1 <local path>` silently
    /// ignores `--depth` for same-filesystem/hardlink clones unless the
    /// source is addressed as a `file://` URL.
    fn shallow_clone_of(src: &Path, depth: &str) -> tempfile::TempDir {
        let dest = tempfile::tempdir().unwrap();
        let url = format!("file://{}", src.display());
        let out = Command::new("git")
            .args(["clone", "-q", "--depth", depth, &url])
            .arg(dest.path())
            .output()
            .expect("git clone runs");
        assert!(out.status.success(), "git clone: {:?}", out.stderr);
        dest
    }

    #[test]
    fn full_clone_with_the_rev_present_is_not_reported_shallow_missing() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        let rev = commit_file(tmp.path(), "fn a() {}");
        assert!(!is_shallow_clone_missing_rev(tmp.path(), &rev).unwrap());
    }

    #[test]
    fn full_clone_missing_the_rev_is_not_a_skip_it_is_a_real_defect() {
        // A full clone missing `rev` must still hard-fail in `materialize`,
        // never be silently skipped by the shallow-checkout escape hatch.
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        commit_file(tmp.path(), "fn a() {}");
        assert!(!is_shallow_clone_missing_rev(tmp.path(), &"0".repeat(40)).unwrap());
    }

    #[test]
    fn shallow_clone_missing_an_older_rev_is_reported_missing() {
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "-q"]);
        let old = commit_file(origin.path(), "fn old() {}");
        commit_file(origin.path(), "fn newer() {}");

        let clone = shallow_clone_of(origin.path(), "1");
        assert!(is_shallow_clone_missing_rev(clone.path(), &old).unwrap());
    }

    #[test]
    fn shallow_clone_holding_the_rev_at_head_is_not_reported_missing() {
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "-q"]);
        let head = commit_file(origin.path(), "fn a() {}");

        let clone = shallow_clone_of(origin.path(), "1");
        assert!(!is_shallow_clone_missing_rev(clone.path(), &head).unwrap());
    }
}
