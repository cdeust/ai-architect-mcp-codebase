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

/// Gives `repo` a deterministic committer identity so `commit_all` never
/// depends on the machine's global git config.
fn init_repo(repo: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@ap.dev"]);
    git(repo, &["config", "user.name", "AP"]);
}

/// Stages everything under `repo` and commits it.
fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--no-gpg-sign", "-m", message]);
}

/// `repo`'s current HEAD sha, for a test that needs to name a commit later.
fn head_sha(repo: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("run git rev-parse");
    assert!(out.status.success(), "git rev-parse HEAD failed");
    String::from_utf8(out.stdout)
        .expect("sha is utf8")
        .trim()
        .to_string()
}

/// Lays out the two directories every test needs — a tool `output_dir` holding
/// `graph/`, and a separate indexed `root` — returning `(output_dir, graph,
/// root)`.
fn scaffold(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let output_dir = tmp.join("out");
    let graph = output_dir.join("graph");
    let root = tmp.join("repo");
    fs::create_dir_all(&graph).expect("mk graph dir");
    fs::create_dir_all(&root).expect("mk repo dir");
    (output_dir, graph, root)
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
    let (output_dir, graph, root) = scaffold(tmp.path());

    crate::query_handlers::write_graph_meta(&output_dir, &root);
    // No file_manifest.json written.
    assert_eq!(check(&graph), json!({"state": "unknown"}));
}

#[test]
fn fresh_when_the_working_tree_matches_the_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
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
    let (output_dir, graph, root) = scaffold(tmp.path());
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
    let (output_dir, graph, root) = scaffold(tmp.path());
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
    let (output_dir, graph, root) = scaffold(tmp.path());
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");
    init_repo(&root);
    commit_all(&root, "initial");

    // write_graph_meta stamps commit_sha == HEAD at this point.
    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    // Move HEAD forward by editing the tracked file and committing.
    fs::write(root.join("a.rs"), b"fn a() { changed(); }\n").expect("edit a.rs");
    commit_all(&root, "second");

    let state = check(&graph);
    assert_eq!(state["state"], json!("stale"), "a.rs's bytes changed");
    assert_eq!(state["dirty_files"], json!(1));
    assert_eq!(state["commits_behind"], json!(1));
    assert_eq!(state["commits_ahead"], json!(0), "HEAD only moved forward");
}

#[test]
fn a_head_that_moved_backward_is_stale_even_with_no_dirty_file() {
    // fleet-watch#112 review finding 2: `git rev-list --count indexed..head`
    // counts one direction only, so a HEAD that moved BACKWARD scores 0 —
    // identical to "HEAD is the indexed commit". Here the checkout rewrites
    // only README.md, which no manifest entry covers, so the file signal stays
    // clean too and the pre-fix guard reported `fresh` for a tree that is not
    // the tree it indexed. The symmetric count names the move.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");
    fs::write(root.join("README.md"), b"one\n").expect("write README");
    init_repo(&root);
    commit_all(&root, "initial");
    let first = head_sha(&root);

    // A commit that leaves every manifested file (a.rs) untouched.
    fs::write(root.join("README.md"), b"two\n").expect("edit README");
    commit_all(&root, "docs only");

    // The graph is indexed HERE, at the second commit.
    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    // HEAD walks BACK one commit. a.rs is byte-identical across the two
    // commits, so git does not rewrite it and its mtime does not move.
    git(&root, &["checkout", "-q", "--detach", &first]);

    let state = check(&graph);
    assert_eq!(
        state["dirty_files"],
        json!(0),
        "the checkout rewrote README.md only — no manifested file changed",
    );
    assert_eq!(
        state["commits_ahead"],
        json!(1),
        "the indexed commit has one commit HEAD does not",
    );
    assert_eq!(
        state["commits_behind"],
        json!(0),
        "nothing was added on top of the indexed commit",
    );
    assert_eq!(
        state["state"],
        json!("stale"),
        "HEAD is not the commit this graph was built from",
    );
}

#[test]
fn commits_behind_is_zero_when_head_is_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");
    init_repo(&root);
    commit_all(&root, "initial");

    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    let state = check(&graph);
    assert_eq!(state["state"], json!("fresh"));
    assert_eq!(state["commits_behind"], json!(0));
}

#[test]
fn a_crafted_commit_sha_never_reaches_git() {
    // fleet-watch#112 review finding 1: `commit_sha` is read from an on-disk
    // sidecar a caller can craft, then interpolated into a `git rev-list`
    // range — so without a guard git EVALUATES it. `HEAD~1` is the proof: it
    // is not an object id at all, yet it resolves to a real count and the
    // response reports a commits-behind number for provenance the sidecar
    // never carried. The same hole admits a `-`-leading value as a flag (arg
    // injection). `is_hex_sha` — the guard `artifact::artifact_staleness`
    // already applies to this same command — rejects it before git runs.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");
    init_repo(&root);
    commit_all(&root, "initial");
    // A second commit, so `HEAD~1..HEAD` would resolve to exactly 1.
    fs::write(root.join("a.rs"), b"fn a() { more(); }\n").expect("edit a.rs");
    commit_all(&root, "second");

    fs::write(
        output_dir.join("meta.json"),
        json!({
            "schema_version": 2,
            "root": root.to_string_lossy(),
            "tool": "ai-architect-mcp-codebase",
            "commit_sha": "HEAD~1",
        })
        .to_string(),
    )
    .expect("write crafted meta");
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    let state = check(&graph);
    assert_eq!(
        state["commits_behind"],
        Value::Null,
        "a non-sha sidecar value must be rejected, never evaluated by git",
    );
}

#[test]
fn a_file_added_since_the_index_is_invisible_to_the_cheap_check() {
    // Documents the deliberate floor-not-ceiling limitation: no directory
    // walk means a wholly new file cannot be seen as a change by this guard.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");

    crate::query_handlers::write_graph_meta(&output_dir, &root);
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    fs::write(root.join("b.rs"), b"fn b() {}\n").expect("write new file b.rs");

    let state = check(&graph);
    assert_eq!(state["state"], json!("fresh"), "new files aren't scanned");
    assert_eq!(state["dirty_files"], json!(0));
}
