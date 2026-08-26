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

    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");
    // No file_manifest.json written.
    assert_eq!(check(&graph), json!({"state": "unknown"}));
}

#[test]
fn fresh_when_the_working_tree_matches_the_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");

    write_manifest_matching_disk(&output_dir, &root, "a.rs");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");

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

    write_manifest_matching_disk(&output_dir, &root, "a.rs");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");

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

    write_manifest_matching_disk(&output_dir, &root, "a.rs");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");

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
    write_manifest_matching_disk(&output_dir, &root, "a.rs");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");

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
    write_manifest_matching_disk(&output_dir, &root, "a.rs");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");

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

    write_manifest_matching_disk(&output_dir, &root, "a.rs");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");

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

    write_manifest_matching_disk(&output_dir, &root, "a.rs");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");

    fs::write(root.join("b.rs"), b"fn b() {}\n").expect("write new file b.rs");

    let state = check(&graph);
    assert_eq!(state["state"], json!("fresh"), "new files aren't scanned");
    assert_eq!(state["dirty_files"], json!(0));
}

#[test]
fn a_manifest_key_that_escapes_the_root_is_never_stat_ed() {
    // fleet-watch#112 review round 3, finding 9. `file_manifest.json` is an
    // on-disk sidecar a caller can craft, and `root.join(rel)` does not
    // constrain `rel`: an ABSOLUTE key discards the root entirely, a `..` key
    // walks out of it. The stat that followed then reported, through
    // `dirty_files`, whether an arbitrary host file's (mtime_ns, size) matched
    // the attacker's guess — an existence-and-attributes oracle, one guess per
    // tool call.
    //
    // This test IS that oracle. The manifest names a file outside the indexed
    // root and records its TRUE mtime and size, so pre-fix the entry compares
    // equal and the graph reads `fresh` with 0 dirty files — the attacker's
    // guess confirmed. Post-fix the key never reaches `stat`, so it counts
    // dirty and the answer carries no information about the target file.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());

    // The target lives outside `root`, as a host file the caller never indexed.
    let outside = tmp.path().join("outside_the_indexed_tree.txt");
    fs::write(&outside, b"contents the caller was never shown\n").expect("write target");
    let target = fs::metadata(&outside).expect("stat target");

    for key in [
        outside.to_string_lossy().to_string(), // absolute: join() drops the root
        format!("../{}", "outside_the_indexed_tree.txt"), // parent-dir traversal
    ] {
        let mut m = manifest::FileManifest::new();
        m.files.insert(
            key.clone(),
            FileState {
                // The attacker's guess, set to the target's real values.
                mtime_ns: manifest::mtime_ns(&target),
                size: target.len(),
                content_hash: String::new(),
            },
        );
        manifest::save(&manifest::manifest_path(&output_dir), &m).expect("save manifest");
        crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");

        let state = check(&graph);
        assert_eq!(
            state["dirty_files"],
            json!(1),
            "key {key:?} must be refused, not resolved against the host filesystem: {state}",
        );
        assert_eq!(
            state["state"],
            json!("stale"),
            "an unverifiable manifest is not evidence of freshness: {state}",
        );
    }
}

#[test]
fn contained_keys_are_exactly_the_relative_ordinary_ones() {
    // The invariant the indexer already satisfies: it writes keys by stripping
    // the root prefix, so every legitimate key is relative and made of ordinary
    // components.
    assert!(is_contained_key("src/main.rs"));
    assert!(is_contained_key("a.rs"));
    assert!(is_contained_key("deep/nested/path/to/file.rs"));

    assert!(!is_contained_key(""), "empty key names nothing");
    assert!(
        !is_contained_key("/etc/hosts"),
        "absolute discards the root"
    );
    assert!(!is_contained_key("../escape.rs"), "parent-dir traversal");
    assert!(
        !is_contained_key("src/../../escape.rs"),
        "traversal mid-path"
    );
    assert!(!is_contained_key("./src/main.rs"), "non-canonical prefix");
}

/// Writes `meta.json` in `output_dir` by hand, so a test can craft the fields
/// `write_graph_meta` would otherwise compute honestly.
fn write_crafted_meta(output_dir: &Path, root: &str, extra: Value) {
    // Record the real manifest identity, so a test that means to exercise some
    // OTHER guard is not silently satisfied by the pairing check instead.
    let paired = fs::metadata(manifest::manifest_path(output_dir)).expect("stat manifest");
    let mut meta = json!({
        "schema_version": 3,
        "root": root,
        "tool": "ai-architect-mcp-codebase",
        "commit_sha": Value::Null,
        "manifest_size": paired.len(),
        "manifest_mtime_ns": manifest::mtime_ns(&paired),
    });
    if let (Some(target), Value::Object(source)) = (meta.as_object_mut(), extra) {
        target.extend(source);
    }
    fs::write(output_dir.join("meta.json"), meta.to_string()).expect("write crafted meta");
}

#[test]
fn a_sidecar_root_naming_a_system_directory_is_refused() {
    // fleet-watch#112 review round 4, finding 1. Round 3 closed this oracle on
    // the manifest-KEY vector; `root` reopened it from the other side of the
    // same file. `"root": "/"` with a perfectly ordinary key — one that passes
    // `is_contained_key` without complaint — resolves through `root.join(rel)`
    // to an absolute system path, and `dirty_files` then reports whether that
    // file's (mtime_ns, size) matched what the attacker wrote in the sidecar.
    //
    // This test IS that oracle, aimed at a file every supported platform has.
    // The manifest records /etc/hosts's REAL mtime and size, so pre-fix the
    // entry compares equal and the receipt reads `fresh`, confirming the guess.
    let host_file = Path::new("/etc/hosts");
    let target = match fs::metadata(host_file) {
        Ok(m) => m,
        // No /etc/hosts (unusual, but do not fake a pass): nothing to assert.
        Err(_) => return,
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, _root) = scaffold(tmp.path());

    let mut m = manifest::FileManifest::new();
    m.files.insert(
        "etc/hosts".to_string(), // relative + ordinary: is_contained_key says yes
        FileState {
            mtime_ns: manifest::mtime_ns(&target),
            size: target.len(),
            content_hash: String::new(),
        },
    );
    manifest::save(&manifest::manifest_path(&output_dir), &m).expect("save manifest");
    write_crafted_meta(&output_dir, "/", json!({}));

    assert_eq!(
        check(&graph),
        json!({"state": "unknown"}),
        "a sidecar naming a system root must never become a join base",
    );
}

#[test]
fn a_root_that_is_not_a_resolvable_directory_is_refused() {
    // The rest of the root policy, asserted directly: relative paths, paths
    // that do not exist, and a file standing where a directory must be.
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("not-a-dir");
    fs::write(&file, b"x").expect("write file");

    assert!(validated_root("relative/path").is_none(), "not absolute");
    assert!(
        validated_root(&tmp.path().join("no/such/dir").to_string_lossy()).is_none(),
        "does not resolve",
    );
    assert!(
        validated_root(&file.to_string_lossy()).is_none(),
        "a file is not an indexed root",
    );
    assert!(
        validated_root(&tmp.path().to_string_lossy()).is_some(),
        "an ordinary existing directory is accepted",
    );
}

#[test]
fn a_deleted_graph_artifact_is_never_reported_fresh() {
    // fleet-watch#112 review round 4, finding 2. `run_*` stamps the receipt onto
    // the error envelope the read tools return when `graph_path` is gone, so
    // without this the caller saw `status: "error"` beside `state: "fresh"` —
    // a receipt contradicting the very answer it is attached to.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");
    write_manifest_matching_disk(&output_dir, &root, "a.rs");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");
    assert_eq!(
        check(&graph)["state"],
        json!("fresh"),
        "precondition: with the artifact present this graph reads fresh",
    );

    fs::remove_dir_all(&graph).expect("delete the graph artifact");

    assert_eq!(
        check(&graph),
        json!({"state": "unknown"}),
        "a graph that is not there cannot be fresh",
    );
}

#[test]
fn sidecars_from_two_different_indexes_are_not_a_verdict() {
    // fleet-watch#112 review round 4, finding 3. The two sidecars are written by
    // two separate atomic operations, so a read landing between them sees one
    // half of the new index and one half of the old. Pairing a fresh commit_sha
    // with a stale manifest reports a just-rebuilt graph as stale.
    //
    // Reproduced without any concurrency: write the manifest, commit it with
    // `meta.json`, then replace the manifest alone — precisely the on-disk state
    // an interrupted index leaves, and the state a reader observes mid-index.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");
    write_manifest_matching_disk(&output_dir, &root, "a.rs");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");
    assert_eq!(
        check(&graph)["state"],
        json!("fresh"),
        "precondition: a matched pair yields a verdict",
    );

    // A later index rewrites the manifest; its `meta.json` has not landed yet.
    fs::write(root.join("b.rs"), b"fn b() {}\n").expect("write b.rs");
    write_manifest_matching_disk(&output_dir, &root, "b.rs");

    assert_eq!(
        check(&graph),
        json!({"state": "unknown"}),
        "half of one index plus half of another is not evidence either way",
    );
}

#[test]
fn a_full_index_leaves_its_two_sidecars_paired() {
    // The write-ORDER half of finding 3, end to end through the real tool. The
    // full-index path used to write `meta.json` BEFORE the manifest, so the
    // sidecar recorded whichever manifest preceded it — on a first index, none
    // at all. Writing the manifest first makes `meta.json` the commit point, and
    // the pairing check below is exactly what fails if that order regresses.
    use crate::test_support::TempDirExt;
    let base = tempfile::Builder::new()
        .prefix("freshness_full_index_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&base);
    let repo = base.join("repo/src");
    let out = base.join("out");
    fs::create_dir_all(&repo).expect("mk repo");
    fs::write(repo.join("a.rs"), "pub fn a() {}\n").expect("write a.rs");

    crate::indexing_handlers::do_index_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "full": true,
    }))
    .expect("index");

    let state = check(&out.join("graph"));
    assert_ne!(
        state["state"],
        json!("unknown"),
        "a just-completed full index must leave a readable, paired pair: {state}",
    );
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn an_empty_manifest_outside_git_verifies_nothing_and_says_so() {
    // fleet-watch#112 review round 4, finding 4. Zero tracked files and no
    // commit provenance means no check ran at all; "fresh" would be a clean
    // bill of health issued without an examination.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
    manifest::save(
        &manifest::manifest_path(&output_dir),
        &manifest::FileManifest::new(),
    )
    .expect("save empty manifest");
    crate::query_handlers::write_graph_meta(&output_dir, &root).expect("write meta");

    assert_eq!(
        check(&graph),
        json!({"state": "unknown"}),
        "nothing was verified, so nothing is attested",
    );
}

#[test]
fn a_symlinked_system_root_is_refused_like_its_literal_form() {
    // fleet-watch#112 review round 5, finding 1. The blacklist compared a
    // CANONICALIZED root against LITERAL strings, and on macOS several of the
    // literals are themselves symlinks: `/etc` canonicalizes to `/private/etc`,
    // which the list never mentions. So `"root": "/etc"` was accepted, and
    // `count_dirty` joined attacker-chosen manifest keys onto it — the same
    // oracle round 4 closed for `"root": "/"`, reopened through a real
    // directory.
    //
    // Asserted against whatever this platform actually does rather than against
    // an assumption about it: only the entries that really are symlinks here
    // exercise the bug, and they must be refused in BOTH forms.
    for literal in crate::query_handlers::FORBIDDEN_GRAPH_PATH_PREFIXES {
        let Ok(canonical) = fs::canonicalize(literal) else {
            continue; // absent on this platform — nothing to assert
        };
        assert!(
            validated_root(literal).is_none(),
            "the literal form must be refused: {literal}",
        );
        assert!(
            validated_root(&canonical.to_string_lossy()).is_none(),
            "the canonical form must be refused too: {literal} -> {}",
            canonical.display(),
        );
    }
}

#[test]
fn a_sidecar_root_reached_through_a_symlink_cannot_become_a_join_base() {
    // The same finding, end to end through `check`, on the concrete host path
    // the review named. Skipped rather than faked where /etc is not a symlink.
    let Ok(canonical) = fs::canonicalize("/etc") else {
        return;
    };
    if canonical == Path::new("/etc") {
        return; // not a symlink on this platform; the literal entry covers it
    }
    let Ok(target) = fs::metadata("/etc/hosts") else {
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, _root) = scaffold(tmp.path());

    let mut m = manifest::FileManifest::new();
    m.files.insert(
        "hosts".to_string(), // relative + ordinary: is_contained_key says yes
        FileState {
            mtime_ns: manifest::mtime_ns(&target),
            size: target.len(),
            content_hash: String::new(),
        },
    );
    manifest::save(&manifest::manifest_path(&output_dir), &m).expect("save manifest");
    write_crafted_meta(&output_dir, "/etc", json!({}));

    assert_eq!(
        check(&graph),
        json!({"state": "unknown"}),
        "/etc canonicalizes to {} and must still be refused",
        canonical.display(),
    );
}

#[test]
fn a_sidecar_cannot_opt_out_of_pairing_by_declaring_an_old_schema() {
    // fleet-watch#112 review round 6, finding 1. `describes_manifest_at` used to
    // return `true` whenever the sidecar declared `schema_version < 3` — a
    // number the sidecar asserts about ITSELF, inside a module whose entire
    // threat model is that the sidecar is attacker-writable. So a forged
    // meta.json carrying nothing but a root and a commit_sha, plus one field
    // saying "I am old", bypassed the whole round-4 pairing defence. No race,
    // no timing, one field.
    //
    // The manifest here describes a tree that has MOVED, so a bypassed pairing
    // does not merely skip a check — it goes on to report a verdict about a
    // graph whose sidecars were never shown to belong together.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (output_dir, graph, root) = scaffold(tmp.path());
    fs::write(root.join("a.rs"), b"fn a() {}\n").expect("write a.rs");
    write_manifest_matching_disk(&output_dir, &root, "a.rs");

    // A sidecar that claims to predate the pairing contract and records no
    // manifest identity at all.
    fs::write(
        output_dir.join("meta.json"),
        json!({
            "schema_version": 2,
            "root": root.to_string_lossy(),
            "tool": "ai-architect-mcp-codebase",
            "commit_sha": Value::Null,
        })
        .to_string(),
    )
    .expect("write forged meta");

    assert_eq!(
        check(&graph),
        json!({"state": "unknown"}),
        "a sidecar that records no manifest identity cannot be paired, whatever \
         version it declares",
    );
}

#[test]
fn analyze_codebase_leaves_a_manifest_that_sees_later_files() {
    // fleet-watch#112 review round 6, finding 2. `analyze_codebase` wrote
    // `meta.json` and never a manifest. It and `index_codebase` are documented
    // as interchangeable entry points over one `output_dir`, so an analyze run
    // on top of an earlier index froze that index's manifest in place: every
    // file added afterwards was permanently invisible to `count_dirty`, and the
    // graph read `fresh` while missing them. No race — just the wrong entry
    // point.
    //
    // The assertion is the consequence, not the mechanism: index, analyze, then
    // CHANGE a file that only the analyze run could have manifested, and require
    // the guard to see it.
    use crate::test_support::TempDirExt;
    let base = tempfile::Builder::new()
        .prefix("freshness_analyze_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&base);
    let repo = base.join("repo/src");
    let out = base.join("out");
    fs::create_dir_all(&repo).expect("mk repo");
    fs::write(repo.join("a.rs"), "pub fn a() {}\n").expect("write a.rs");

    let args = json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
    });
    crate::indexing_handlers::do_index_codebase(&args).expect("index");

    // A file that exists only from the analyze run's point of view.
    fs::write(repo.join("b.rs"), "pub fn b() {}\n").expect("write b.rs");
    crate::analyze_handlers::do_analyze_codebase(&args).expect("analyze");

    let graph = out.join("graph");
    assert_eq!(
        check(&graph)["state"],
        json!("fresh"),
        "precondition: straight after analyze the graph describes the tree",
    );

    fs::write(repo.join("b.rs"), "pub fn b() { changed(); }\n").expect("edit b.rs");
    let state = check(&graph);
    assert_eq!(
        state["state"],
        json!("stale"),
        "analyze must leave a manifest that covers b.rs, or edits to it are \
         invisible forever: {state}",
    );
    let _ = fs::remove_dir_all(&base);
}
