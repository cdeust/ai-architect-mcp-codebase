//! Issue #443, reviews of #478: which changes to the tree make the next
//! `analyze_codebase` rebuild, and which do not.

use super::tests::{analyze, fixture, outcome};
use crate::test_support::TempDirExt;
use serde_json::{json, Value};
use std::fs;

#[test]
fn a_call_after_an_edit_rebuilds_and_matches_a_fresh_analysis() {
    let (repo, base) = fixture("analyze443_edit_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));

    // Change a call target, add a file, delete one.
    fs::write(
        repo.join("src/a.rs"),
        "pub fn alpha() { crate::b::gamma(); }\n",
    )
    .expect("edit");
    fs::write(
        repo.join("src/c.rs"),
        "pub fn delta() { crate::b::beta(); }\n",
    )
    .expect("add");
    fs::write(
        repo.join("src/lib.rs"),
        "mod a;\nmod b;\nmod c;\npub fn root() { a::alpha(); }\n",
    )
    .expect("edit lib.rs");
    fs::remove_file(repo.join("src/extra/v.rs")).expect("delete");

    // An edited tree is rebuilt: the resolver cannot invalidate edges of the
    // unchanged files yet (see the module doc of `index_phase`).
    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["index"]["mode"], "full", "{second}");

    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&second), outcome(&fresh));
}

#[test]
fn a_call_made_ambiguous_by_an_added_file_is_rebuilt() {
    // Codex review of #478, P1: `a.rs` (unchanged) calls `beta()`, unique in
    // the first run. An added file declares a second `beta`, so a fresh
    // analysis no longer resolves the call by unique match. Reusing the graph
    // kept the earlier edge (3 call-site targets against 2), so a change of
    // the tree must rebuild.
    let (repo, base) = fixture("analyze443_ambiguous_");
    let out = base.join("out");
    fs::write(repo.join("src/a.rs"), "pub fn alpha() { beta(); }\n").expect("edit a.rs");
    analyze(&repo, &out, json!({}));
    fs::write(repo.join("src/c.rs"), "pub fn beta() {}\n").expect("add c.rs");
    fs::write(
        repo.join("src/lib.rs"),
        "mod a;\nmod b;\nmod c;\npub fn root() { a::alpha(); }\n",
    )
    .expect("edit lib.rs");

    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["index"]["mode"], "full", "{second}");
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&second), outcome(&fresh));
}

#[test]
fn an_index_codebase_pass_that_applied_changes_makes_the_next_analysis_rebuild() {
    // Claude review of #478: a same-settings `index_codebase` pass between two
    // analyses applied an add and kept the scope, so the next analysis saw no
    // change and reused a graph whose resolver rows were stale.
    let (repo, base) = fixture("analyze443_index_between_");
    let out = base.join("out");
    fs::write(repo.join("src/a.rs"), "pub fn alpha() { beta(); }\n").expect("edit a.rs");
    analyze(&repo, &out, json!({}));
    fs::write(repo.join("src/c.rs"), "pub fn beta() {}\n").expect("add c.rs");
    crate::indexing_handlers::do_index_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "language": "rust",
    }))
    .expect("index");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&resp), outcome(&fresh));
}

#[test]
fn a_pure_add_delete_or_rename_rebuilds() {
    // Each case touches one file that no `mod` declares, so only the matching
    // arm of `tree_changed_since` can see it (review of #478).
    let (repo, base) = fixture("analyze443_pure_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let extra = repo.join("src/extra");

    fs::write(extra.join("added.rs"), "pub fn added() {}\n").expect("add");
    assert_eq!(
        analyze(&repo, &out, json!({}))["index"]["mode"],
        "full",
        "add"
    );

    fs::rename(extra.join("added.rs"), extra.join("renamed.rs")).expect("rename");
    assert_eq!(
        analyze(&repo, &out, json!({}))["index"]["mode"],
        "full",
        "rename"
    );

    fs::remove_file(extra.join("renamed.rs")).expect("delete");
    assert_eq!(
        analyze(&repo, &out, json!({}))["index"]["mode"],
        "full",
        "delete"
    );
}

#[test]
fn a_touched_file_with_identical_content_is_still_reused() {
    // A moved mtime with the same bytes is not a change (the hash fallback);
    // otherwise every `git checkout` would rebuild.
    let (repo, base) = fixture("analyze443_touch_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let b = repo.join("src/b.rs");
    let bytes = fs::read(&b).expect("read b.rs");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    fs::write(&b, &bytes).expect("rewrite b.rs");
    fs::File::options()
        .write(true)
        .open(&b)
        .and_then(|f| f.set_modified(later))
        .expect("move mtime");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "incremental", "{resp}");
    assert_eq!(resp["index"]["files_reparsed"], 0, "{resp}");
}

#[test]
fn a_file_unreadable_at_build_time_is_retried() {
    // Review of #478: an empty recorded hash means the full index could not
    // read the file; `chmod +r` moves no mtime, so it must count as changed.
    let (repo, base) = fixture("analyze443_unreadable_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let path = crate::indexer::manifest::manifest_path(&out);
    let mut m: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
    m["files"]["src/b.rs"]["content_hash"] = json!("");
    fs::write(&path, m.to_string()).expect("write");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
fn an_in_place_edit_of_the_same_size_rebuilds() {
    // Review of #478: every other edit test also adds a file, so the hash arm
    // of the change check was never reached with a real content change.
    let (repo, base) = fixture("analyze443_same_size_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let b = repo.join("src/b.rs");
    let before = fs::read_to_string(&b).expect("read");
    let after = before.replacen("beta", "bxta", 1);
    assert_eq!(before.len(), after.len(), "precondition: same size");
    fs::write(&b, after).expect("edit");
    // Explicitly later: on a coarse-mtime filesystem the write could land in
    // the same tick, and the test would never reach the hash arm.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    fs::File::options()
        .write(true)
        .open(&b)
        .and_then(|f| f.set_modified(later))
        .expect("bump mtime");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&resp), outcome(&fresh));
}

#[test]
fn full_manifest_and_the_change_check_see_an_edit_made_after_the_snapshot() {
    // Review of #478: the property the full paths' ordering relies on (a
    // snapshot taken before an edit reports it; one taken after hides it).
    // The production ordering itself is pinned end to end in
    // `index_phase_rebuild_tests.rs`.
    let (repo, _base) = fixture("analyze443_snapshot_order_");
    let opts = crate::indexer::IndexOptions::default();
    let snapshot = crate::indexer::full_manifest(&repo, &opts).expect("snapshot");
    let b = repo.join("src/b.rs");
    fs::write(&b, "pub fn beta() { edited(); }\n").expect("edit during the index");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    fs::File::options()
        .write(true)
        .open(&b)
        .and_then(|f| f.set_modified(later))
        .expect("bump mtime");
    assert!(
        crate::indexer::tree_changed_since(&repo, &opts, &snapshot).expect("check"),
        "the pre-index snapshot must not vouch for the edited file"
    );
    let after = crate::indexer::full_manifest(&repo, &opts).expect("post-index manifest");
    assert!(
        !crate::indexer::tree_changed_since(&repo, &opts, &after).expect("check"),
        "a manifest taken after the edit would hide it: the ordering is what matters"
    );
}

#[test]
fn an_iac_file_the_iac_pass_cannot_read_keeps_its_count_on_reuse() {
    // Codex review of #478: the main pass indexes a Dockerfile that is not
    // UTF-8 (its File node is written), the IaC pass records it as skipped,
    // and the full receipt counts it. The reused receipt counted it out.
    let base = tempfile::Builder::new()
        .prefix("analyze443_iac_skip_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let repo = base.join("repo");
    fs::create_dir_all(&repo).expect("mk repo");
    fs::write(repo.join("Dockerfile"), b"FROM \xff\xfe scratch\n").expect("write Dockerfile");
    let out = base.join("out");
    let args = json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
    });
    let first = crate::analyze_handlers::do_analyze_codebase(&args).expect("first");
    assert_eq!(first["index"]["mode"], "full", "{first}");
    assert_eq!(first["coverage"]["files_indexed"], 1, "{first}");
    let second = crate::analyze_handlers::do_analyze_codebase(&args).expect("second");
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert_eq!(
        second["coverage"]["files_indexed"], first["coverage"]["files_indexed"],
        "{second}"
    );
}

/// Issue #448 met #443 on merge: `full_ast` decides what a parse persists, so
/// a graph built without the layer must not answer a call that asks for it.
#[test]
fn a_changed_full_ast_rebuilds() {
    let (repo, base) = fixture("analyze443_full_ast_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let with_ast = analyze(&repo, &out, json!({ "full_ast": true }));
    assert_eq!(with_ast["index"]["mode"], "full", "{with_ast}");
    let same = analyze(&repo, &out, json!({ "full_ast": true }));
    assert_eq!(same["index"]["mode"], "incremental", "{same}");
    let without = analyze(&repo, &out, json!({}));
    assert_eq!(without["index"]["mode"], "full", "{without}");
}
