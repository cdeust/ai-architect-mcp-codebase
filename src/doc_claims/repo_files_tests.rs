use super::*;

fn repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("repo");
    fs::create_dir_all(root.join("docs")).expect("docs");
    fs::write(root.join("README.md"), "# t\n\nIt has 3 tests.\nsecond\n").expect("readme");
    fs::write(root.join("docs/a.md"), "a\n").expect("a");
    fs::write(tmp.path().join("secret.md"), "outside\n").expect("secret");
    tmp
}

#[test]
fn a_file_inside_the_root_is_read_once_and_cached() {
    let tmp = repo();
    let mut files = RepoFiles::new(&tmp.path().join("repo")).expect("root");
    assert!(files.read("README.md").expect("read").contains("3 tests"));
    fs::remove_file(tmp.path().join("repo/README.md")).expect("rm");
    assert!(
        files.read("README.md").is_ok(),
        "second read is served from the cache"
    );
    assert!(files.read("./docs/a.md").is_ok());
}

#[test]
fn paths_that_leave_the_root_are_refused_before_any_read() {
    let tmp = repo();
    let mut files = RepoFiles::new(&tmp.path().join("repo")).expect("root");
    let absolute = tmp.path().join("secret.md");
    for (rel, reason) in [
        ("../secret.md", "path_leaves_repo_root"),
        ("docs/../../secret.md", "path_leaves_repo_root"),
        (absolute.to_str().expect("utf8"), "path_absolute"),
        ("", "path_empty_or_invalid"),
    ] {
        assert_eq!(files.read(rel).unwrap_err().0, reason, "{rel}");
    }
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_resolving_outside_the_root_is_refused() {
    let tmp = repo();
    let root = tmp.path().join("repo");
    std::os::unix::fs::symlink(tmp.path().join("secret.md"), root.join("link.md")).expect("ln");
    let mut files = RepoFiles::new(&root).expect("root");
    assert_eq!(
        files.read("link.md").unwrap_err().0,
        "path_leaves_repo_root"
    );
    assert_eq!(
        files.exists("link.md").unwrap_err().0,
        "path_leaves_repo_root"
    );
}

#[test]
fn a_missing_file_is_reported_as_missing() {
    let tmp = repo();
    let files = RepoFiles::new(&tmp.path().join("repo")).expect("root");
    assert_eq!(files.exists("nope.md"), Ok(false));
    assert_eq!(files.exists("docs/a.md"), Ok(true));
}

#[test]
fn the_anchor_must_be_verbatim_on_the_named_line() {
    let content = "# t\n\nIt has 3 tests.\nsecond\n";
    assert_eq!(anchor_holds(content, 3, "3 tests"), Ok(()));
    assert_eq!(anchor_holds(content, 3, "It has 3 tests.\nsecond"), Ok(()));
    assert_eq!(
        anchor_holds(content, 2, "3 tests"),
        Err("text_not_at_line".into())
    );
    assert_eq!(
        anchor_holds(content, 3, "3  tests"),
        Err("text_not_at_line".into())
    );
    assert!(anchor_holds(content, 0, "3 tests")
        .unwrap_err()
        .starts_with("line_out_of_range"));
    assert!(anchor_holds(content, 9, "3 tests")
        .unwrap_err()
        .starts_with("line_out_of_range"));
    assert_eq!(anchor_holds(content, 3, "  "), Err("text_empty".into()));
}
