// walk::tests — issue #249 unit coverage for the walk-level `exclude_dirs`
// contract and graceful degrade on `PermissionDenied`. Extracted into its own
// file (Fowler "Extract Class") so `indexer/walk.rs` stays under the §4.1
// file cap as these knobs grow — the same pattern `indexer/tests.rs` already
// applies to `indexer/mod.rs`.

use super::*;
use crate::test_support::TempDirExt;

fn names(root: &Path, opts: WalkOptions) -> Vec<String> {
    let mut v: Vec<String> = collect_source_files(root, opts)
        .unwrap()
        .files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Fixture: `root/app.rs`, `root/secrets/key.rs`, `root/libs/secrets/dep.rs`
/// (a same-named `secrets` dir nested elsewhere — NOT under a built-in
/// skip-listed name like `vendor`/`node_modules`, so it is reached by the
/// walk regardless of `exclude_dirs`), `root/config/secrets/tok.rs`.
fn build_exclude_fixture() -> crate::test_support::TestTempDir {
    let root = tempfile::Builder::new()
        .prefix("walk_exclude_test_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("secrets")).unwrap();
    std::fs::create_dir_all(root.join("libs/secrets")).unwrap();
    std::fs::create_dir_all(root.join("config/secrets")).unwrap();
    std::fs::write(root.join("app.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("secrets/key.rs"), "fn key() {}\n").unwrap();
    std::fs::write(root.join("libs/secrets/dep.rs"), "fn dep() {}\n").unwrap();
    std::fs::write(root.join("config/secrets/tok.rs"), "fn tok() {}\n").unwrap();
    root
}

#[test]
fn bare_name_excludes_every_matching_dir_anywhere_in_the_tree() {
    // A bare name (no path separator) prunes ALL "secrets" dirs: the
    // top-level one, config/secrets, AND libs/secrets.
    let root = build_exclude_fixture();
    let opts = WalkOptions {
        language_filter: None,
        dependency_scope: DependencyScope::None,
        exclude_dirs: ExcludeSet::new(&["secrets".to_string()]),
    };
    assert_eq!(names(&root, opts), vec!["app.rs"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn relative_path_excludes_exactly_one_subtree() {
    // A relative-path entry ("config/secrets") prunes ONLY that one subtree;
    // the top-level "secrets" and "libs/secrets" dirs (same bare name,
    // different path) survive.
    let root = build_exclude_fixture();
    let opts = WalkOptions {
        language_filter: None,
        dependency_scope: DependencyScope::None,
        exclude_dirs: ExcludeSet::new(&["config/secrets".to_string()]),
    };
    let mut got = names(&root, opts);
    got.sort();
    assert_eq!(
        got,
        vec!["app.rs", "dep.rs", "key.rs"],
        "only config/secrets is pruned — the same-named top-level and \
         libs/ dirs survive"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn exclusion_wins_over_dependency_scope_full() {
    // Fixture: root/app.rs, root/node_modules/dep.rs (would normally be
    // descended under DependencyScope::Full), root/node_modules/secret/x.rs.
    let root = tempfile::Builder::new()
        .prefix("walk_exclude_full_test_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("node_modules/secret")).unwrap();
    std::fs::write(root.join("app.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("node_modules/dep.rs"), "fn dep() {}\n").unwrap();
    std::fs::write(root.join("node_modules/secret/x.rs"), "fn x() {}\n").unwrap();

    // Full descends into node_modules normally...
    let full_no_exclude = WalkOptions {
        language_filter: None,
        dependency_scope: DependencyScope::Full,
        exclude_dirs: ExcludeSet::default(),
    };
    assert_eq!(
        names(&root, full_no_exclude),
        vec!["app.rs", "dep.rs", "x.rs"]
    );

    // ...but exclude_dirs prunes "secret" even under Full — exclusion wins
    // over every dependency_scope tier (issue #249).
    let full_with_exclude = WalkOptions {
        language_filter: None,
        dependency_scope: DependencyScope::Full,
        exclude_dirs: ExcludeSet::new(&["secret".to_string()]),
    };
    assert_eq!(names(&root, full_with_exclude), vec!["app.rs", "dep.rs"]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn excluded_dirs_are_reported_and_never_reach_is_dependency_path() {
    // The walk reports every pruned dir (never silently dropped), and no
    // file under it is ever collected — so `is_dependency_path` (the
    // PublicApi visibility filter's scoping check) is never even called for
    // it, because it is only invoked per file the walk returns.
    let root = build_exclude_fixture();
    let opts = WalkOptions {
        language_filter: None,
        dependency_scope: DependencyScope::None,
        exclude_dirs: ExcludeSet::new(&["secrets".to_string()]),
    };
    let outcome = collect_source_files(&root, opts).unwrap();
    assert_eq!(
        outcome.files.len(),
        1,
        "only app.rs survives: {:?}",
        outcome.files
    );
    let mut excluded = outcome.excluded_dirs.clone();
    excluded.sort();
    assert_eq!(
        excluded,
        vec!["config/secrets", "libs/secrets", "secrets"],
        "every matching dir is reported, not silently dropped"
    );
    assert!(outcome.unreadable_dirs.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn equivalent_relative_path_spellings_normalize_to_the_same_set() {
    // PR #250 review, BLOCK finding: "./config/secrets" passed boundary
    // validation but never matched dir_rel()'s "config/secrets" — the
    // exclusion silently did nothing. Every syntactically-equivalent
    // spelling of one subtree must canonicalize to the same set.
    let canonical = ExcludeSet::new(&["config/secrets".to_string()]);
    for spelling in [
        "./config/secrets",
        "config//secrets",
        ".//config/secrets/",
        "config\\secrets",
        " config/secrets ",
    ] {
        assert_eq!(
            ExcludeSet::new(&[spelling.to_string()]),
            canonical,
            "{spelling:?} must canonicalize to the same subtree entry"
        );
    }
    // A separator-carrying entry that canonicalizes to ONE component is
    // still a PATH entry pinned to the root-level subtree — not a bare name
    // matched anywhere in the tree.
    assert_eq!(
        ExcludeSet::new(&["./secrets".to_string()]),
        ExcludeSet::new(&["secrets/".to_string()]),
    );
    assert_ne!(
        ExcludeSet::new(&["./secrets".to_string()]),
        ExcludeSet::new(&["secrets".to_string()]),
        "a ./-prefixed entry pins the root subtree; a bare name matches anywhere"
    );
}

#[test]
fn dot_prefixed_relative_path_excludes_exactly_the_subtree() {
    // End-to-end pin for the same BLOCK finding: the natural "./"-prefixed
    // spelling must prune the subtree at the walk, like its canonical form.
    let root = build_exclude_fixture();
    let opts = WalkOptions {
        language_filter: None,
        dependency_scope: DependencyScope::None,
        exclude_dirs: ExcludeSet::new(&["./config/secrets".to_string()]),
    };
    let mut got = names(&root, opts);
    got.sort();
    assert_eq!(
        got,
        vec!["app.rs", "dep.rs", "key.rs"],
        "./config/secrets must prune exactly the config/secrets subtree"
    );
}

/// Restores 0o755 on drop so the fixture stays removable — including when a
/// panicking assertion unwinds past the locked directory.
#[cfg(unix)]
struct RestorePerms(std::path::PathBuf);

#[cfg(unix)]
impl Drop for RestorePerms {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Fixture: `root/app.rs` plus `root/locked/secret.rs` with `locked/`
/// chmod'd to 000. Returns the root guard and the locked directory's path.
#[cfg(unix)]
fn build_locked_fixture() -> (crate::test_support::TestTempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::Builder::new()
        .prefix("walk_permdenied_test_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    std::fs::create_dir_all(root.join("locked")).unwrap();
    std::fs::write(root.join("app.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("locked/secret.rs"), "fn secret() {}\n").unwrap();
    let locked_dir = root.join("locked");
    std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    (root, locked_dir)
}

/// Regression for issue #249: a locked-down (chmod 000) subdirectory must
/// not abort the whole walk. Pre-fix, `walk_dir_recursive` propagated ANY
/// `read_dir` error (including EACCES) as a hard `Err`, discarding every
/// file already collected. Post-fix, the directory is recorded as
/// unreadable and the walk continues past it.
#[test]
#[cfg(unix)]
fn permission_denied_directory_is_skipped_and_reported_not_fatal() {
    let (root, locked_dir) = build_locked_fixture();
    // Declared after `root` so it drops FIRST: perms restored, then cleanup.
    let _restore = RestorePerms(locked_dir.clone());

    // Precondition guard: running as root (or on a filesystem that ignores
    // the mode bit) makes read_dir succeed anyway — skip rather than assert
    // a false failure in that environment.
    if std::fs::read_dir(&locked_dir).is_ok() {
        eprintln!(
            "permission_denied_directory_is_skipped_and_reported_not_fatal: \
             read_dir on a chmod-000 dir unexpectedly succeeded (running as \
             root?) — skipping this test's assertions"
        );
        return;
    }

    let outcome = collect_source_files(&root, WalkOptions::default())
        .expect("a PermissionDenied subdirectory must not abort the walk");
    assert_eq!(
        outcome.files.len(),
        1,
        "app.rs must still be collected despite the locked sibling dir: {:?}",
        outcome.files
    );
    assert_eq!(outcome.files[0].file_name().unwrap(), "app.rs");
    assert_eq!(
        outcome.unreadable_dirs,
        vec!["locked".to_string()],
        "the locked directory must be reported, not silently dropped"
    );
    assert!(outcome.excluded_dirs.is_empty());
}
