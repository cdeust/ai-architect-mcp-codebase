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
/// precondition:  `repo_hint` is a directory inside a git work tree or bare
///                repo; `rev` names a commit (or tree-ish) present in that
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
    let treeish = format!("{rev}:{subdir}");
    let mut archive = Command::new("git")
        .arg("-C")
        .arg(&top)
        .args(["archive", "--format=tar", &treeish])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn `git archive`: {e}"))?;
    let tar_stdin = archive.stdout.take().ok_or("git archive: no stdout")?;
    let untar = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(&source_dir)
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

/// Root of the git work tree (or the git dir of a bare repo) holding `hint`.
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
}
