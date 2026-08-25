//! fleet-watch#112: query-time staleness guard on read tools.
use super::*;
use std::fs;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Writes `file_manifest.json` in `output_dir` matching the CURRENT on-disk
/// state of `rel_path` under `root` — i.e. a manifest that, at the moment of
/// writing, describes a fresh graph.
fn write_manifest_matching_disk(output_dir: &Path, root: &Path, rel_path: &str) {
    let abs = root.join(rel_path);
    let meta = fs::metadata(&abs).expect("stat fixture file");
    let mut m = manifest::FileManifest::new();
    m.files.insert(
        rel_path.to_string(),
        FileState {
            mtime_ns: manifest::mtime_ns(&meta),
            size: meta.len(),
            content_hash: manifest::hash_file(&abs).expect("hash fixture file"),
        },
    );
    manifest::save(&manifest::manifest_path(output_dir), &m).expect("save manifest");
}

#[test]
fn unknown_without_a_meta_sidecar() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let graph = tmp.path().join("out/graph");
    fs::create_dir_all(&graph).expect("mk graph dir");
    // No meta.json, no file_manifest.json in "out/".
    assert_eq!(check(&graph), json!({"state": "unknown"}));
}

#[test]
fn unknown_when_meta_exists_but_manifest_does_not() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join("out");
    let graph = output_dir.join("graph");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&graph).expect("mk graph dir");
    fs::create_dir_all(&root).expect("mk repo dir");

    crate::query_handlers::write_graph_meta(&output_dir, &root);
    // No file_manifest.json written.
    assert_eq!(check(&graph), json!({"state": "unknown"}));
}

#[test]
fn fresh_when_the_working_tree_matches_the_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join("out");
    let graph = output_dir.join("graph");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&graph).expect("mk graph dir");
    fs::create_dir_all(&root).expect("mk repo dir");
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");

    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    let state = check(&graph);
    assert_eq!(state["state"], json!("fresh"));
    assert_eq!(state["dirty_files"], json!(0));
    assert_eq!(state["checked_files"], json!(1));
    // root is not a git working tree → no commit provenance to compare.
    assert_eq!(state["commits_behind"], Value::Null);
}

#[test]
fn stale_when_a_manifested_file_changes_on_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join("out");
    let graph = output_dir.join("graph");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&graph).expect("mk graph dir");
    fs::create_dir_all(&root).expect("mk repo dir");
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");

    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    // Edit the file after the manifest was captured.
    fs::write(root.join("a.rs"), b"fn a() { changed(); }\n").expect("edit a.rs");

    let state = check(&graph);
    assert_eq!(state["state"], json!("stale"));
    assert_eq!(state["dirty_files"], json!(1));
}

#[test]
fn stale_when_a_manifested_file_is_deleted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join("out");
    let graph = output_dir.join("graph");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&graph).expect("mk graph dir");
    fs::create_dir_all(&root).expect("mk repo dir");
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");

    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    fs::remove_file(root.join("a.rs")).expect("delete a.rs");

    let state = check(&graph);
    assert_eq!(state["state"], json!("stale"));
    assert_eq!(state["dirty_files"], json!(1));
}

#[test]
fn commits_behind_is_reported_in_a_git_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join("out");
    let graph = output_dir.join("graph");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&graph).expect("mk graph dir");
    fs::create_dir_all(&root).expect("mk repo dir");
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "t@ap.dev"]);
    git(&root, &["config", "user.name", "AP"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "--no-gpg-sign", "-m", "initial"]);

    // write_graph_meta stamps commit_sha == HEAD at this point.
    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    // Move HEAD forward by editing the tracked file and committing.
    fs::write(root.join("a.rs"), b"fn a() { changed(); }\n").expect("edit a.rs");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "--no-gpg-sign", "-m", "second"]);

    let state = check(&graph);
    assert_eq!(state["state"], json!("stale"), "a.rs's bytes changed");
    assert_eq!(state["dirty_files"], json!(1));
    assert_eq!(state["commits_behind"], json!(1));
}

#[test]
fn commits_behind_is_zero_when_head_is_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join("out");
    let graph = output_dir.join("graph");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&graph).expect("mk graph dir");
    fs::create_dir_all(&root).expect("mk repo dir");
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "t@ap.dev"]);
    git(&root, &["config", "user.name", "AP"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "--no-gpg-sign", "-m", "initial"]);

    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    let state = check(&graph);
    assert_eq!(state["state"], json!("fresh"));
    assert_eq!(state["commits_behind"], json!(0));
}

#[test]
fn a_file_added_since_the_index_is_invisible_to_the_cheap_check() {
    // Documents the deliberate floor-not-ceiling limitation: no directory
    // walk means a wholly new file cannot be seen as a change by this guard.
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join("out");
    let graph = output_dir.join("graph");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&graph).expect("mk graph dir");
    fs::create_dir_all(&root).expect("mk repo dir");
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");

    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    fs::write(root.join("b.rs"), b"fn b() {}\n").expect("write new file b.rs");

    let state = check(&graph);
    assert_eq!(state["state"], json!("fresh"), "new files aren't scanned");
    assert_eq!(state["dirty_files"], json!(0));
}
