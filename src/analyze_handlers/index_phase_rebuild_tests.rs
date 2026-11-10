//! Issue #443, reviews of #478: what a full rebuild does before and after the
//! index, driven through the production entry points (`do_analyze_codebase`,
//! `do_index_codebase`), and the Cargo map a reuse compares.

use super::tests::{analyze, fixture, outcome};
use crate::analyze_handlers::do_analyze_codebase;
use crate::indexing_handlers::do_index_codebase;
use crate::test_support::{TempDirExt, TestTempDir};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

type Entry = fn(&Value) -> Result<Value, String>;

fn full_args(repo: &Path, out: &Path) -> Value {
    json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "language": "rust",
        "full": true,
    })
}

fn graph_sidecars_intact(out: &Path) {
    assert!(
        crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(out)).is_some(),
        "manifest kept"
    );
    assert!(out.join("meta.json").exists(), "meta.json kept");
    assert!(
        crate::indexer::coverage::load(&crate::indexer::coverage::coverage_path(out)).is_some(),
        "coverage sidecar kept"
    );
    assert!(
        fs::symlink_metadata(out.join("graph")).is_ok(),
        "graph kept"
    );
}

#[test]
fn an_over_cap_tree_is_refused_before_the_old_graph_is_touched() {
    // Review of #478: `prepare_full_rebuild` refuses a tree over the byte cap
    // as its first step, so the previous graph and its sidecars survive.
    let (repo, base) = fixture("analyze443_cap_keeps_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let opts = crate::indexer::IndexOptions {
        language_filter: Some(crate::parser::Language::Rust),
        ..Default::default()
    };
    let err = crate::query_handlers::prepare_full_rebuild_within(
        &repo,
        &out,
        &out.join("graph"),
        &opts,
        Some(1),
    )
    .expect_err("1-byte cap");
    assert!(
        err.starts_with(crate::indexer::TOTAL_BYTES_EXCEEDED),
        "{err}"
    );
    graph_sidecars_intact(&out);
}

#[test]
fn any_snapshot_error_is_refused_before_the_old_graph_is_touched() {
    // Claude review of #478: not only the byte cap. A root the walk cannot
    // read fails the index too, so it must fail before the old graph goes.
    let (repo, base) = fixture("analyze443_snap_err_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let gone = base.join("not-there");
    let err = crate::query_handlers::prepare_full_rebuild(
        &gone,
        &out,
        &out.join("graph"),
        &crate::indexer::IndexOptions::default(),
    )
    .expect_err("an unreadable root");
    assert!(!err.is_empty());
    graph_sidecars_intact(&out);
}

fn refuse_release(_path: &Path) -> Result<(), String> {
    Err("graph_handle_in_use: held by a running request (test)".into())
}

/// Review of #478: handles are released before the sidecars are forgotten,
/// so a busy graph fails while its manifest, meta.json and coverage sidecar
/// still describe it.
/// The hook is per thread; each test runs on its own.
fn busy_graph_keeps_its_sidecars(entry: Entry, prefix: &str) {
    let (repo, base) = fixture(prefix);
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    crate::graph_store::register_release_hook(refuse_release);
    let err = entry(&full_args(&repo, &out)).expect_err("the graph is busy");
    assert!(err.contains("graph_handle_in_use"), "{err}");
    graph_sidecars_intact(&out);
}

#[test]
fn a_busy_graph_keeps_its_sidecars_when_analyze_is_refused() {
    busy_graph_keeps_its_sidecars(do_analyze_codebase, "analyze443_busy_");
}

#[test]
fn a_busy_graph_keeps_its_sidecars_when_index_codebase_is_refused() {
    busy_graph_keeps_its_sidecars(do_index_codebase, "analyze443_busy_index_");
}

thread_local! {
    /// The file `edit_on_first_release` edits, armed by the test.
    static EDIT_ON_RELEASE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// A release hook that edits the armed file once, with a later mtime, then
/// lets every release through. `prepare_full_rebuild` releases after its
/// snapshot and before the index, so the edit lands during the rebuild.
fn edit_on_first_release(_path: &Path) -> Result<(), String> {
    if let Some(file) = EDIT_ON_RELEASE.with(|c| c.borrow_mut().take()) {
        fs::write(&file, "pub fn beta() { edited(); }\n").map_err(|e| e.to_string())?;
        let later = SystemTime::now() + Duration::from_secs(120);
        fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|f| f.set_modified(later))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Claude review of #478: the production full path, not a hand-built copy of
/// its steps. A file edited during the rebuild must make the next analysis
/// rebuild: the manifest that path saves is the snapshot taken before the
/// index, not one taken after it.
fn an_edit_during_a_full_rebuild_is_seen_next_time(entry: Entry, prefix: &str) {
    let (repo, base) = fixture(prefix);
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    EDIT_ON_RELEASE.with(|c| *c.borrow_mut() = Some(repo.join("src/b.rs")));
    crate::graph_store::register_release_hook(edit_on_first_release);
    entry(&full_args(&repo, &out)).expect("full rebuild");
    assert!(
        EDIT_ON_RELEASE.with(|c| c.borrow().is_none()),
        "the edit ran during the rebuild"
    );
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
fn an_edit_during_a_full_analysis_is_seen_by_the_next_one() {
    an_edit_during_a_full_rebuild_is_seen_next_time(do_analyze_codebase, "analyze443_during_");
}

#[test]
fn an_edit_during_a_full_index_codebase_is_seen_by_the_next_analysis() {
    an_edit_during_a_full_rebuild_is_seen_next_time(do_index_codebase, "analyze443_during_ix_");
}

#[test]
fn a_graph_from_a_full_index_codebase_is_reused_by_the_next_analysis() {
    // Positive control for the `do_index_codebase` tests above (Claude review
    // of #478): without the failure each of them injects, that path leaves a
    // manifest analyze reuses, so their `mode == "full"` is the failure's doing.
    let (repo, base) = fixture("analyze443_ix_reuse_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    do_index_codebase(&full_args(&repo, &out)).expect("full index");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "incremental", "{resp}");
}

#[test]
fn a_full_index_codebase_whose_coverage_write_fails_is_not_reused() {
    // Claude review of #478: that path only logs a failed coverage write. The
    // previous graph's report must not survive beside the new graph, or the
    // next analysis reuses it and carries its gaps forward.
    let (repo, base) = fixture("analyze443_ix_covfail_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let cov = crate::indexer::coverage::coverage_path(&out);
    let blocker = cov.with_extension("json.tmp");
    fs::create_dir_all(&blocker).expect("block the temp file");
    do_index_codebase(&full_args(&repo, &out)).expect("the index itself succeeds");
    assert!(
        crate::indexer::coverage::load(&cov).is_none(),
        "the previous graph's coverage report is gone"
    );
    fs::remove_dir(&blocker).expect("unblock");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
fn a_missing_coverage_report_is_not_turned_into_a_clean_one() {
    // Claude review of #478: after a full `index_codebase` whose coverage
    // write failed, an incremental pass on the unchanged tree found no report
    // to carry forward and wrote one with no gaps. This sequence is closed by
    // the full path dropping its scope (asserted below); the incremental
    // pass's own rule (`prior_loaded`) is pinned by
    // `an_incremental_pass_without_a_prior_report_records_no_scope`.
    let (repo, base) = fixture("analyze443_cov_chain_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let blocker = crate::indexer::coverage::coverage_path(&out).with_extension("json.tmp");
    fs::create_dir_all(&blocker).expect("block the temp file");
    do_index_codebase(&full_args(&repo, &out)).expect("full index");
    let path = crate::indexer::manifest::manifest_path(&out);
    let m = crate::indexer::manifest::load(&path).expect("manifest");
    assert!(m.scope.is_none(), "no scope beside a missing report");
    fs::remove_dir(&blocker).expect("unblock");
    let mut args = full_args(&repo, &out);
    args["full"] = json!(false);
    do_index_codebase(&args).expect("incremental index");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
fn an_incremental_pass_without_a_prior_report_records_no_scope() {
    // The same rule inside `index_incremental`: with no report to carry
    // forward, the one it writes lost the unchanged files' gaps.
    let (repo, base) = fixture("analyze443_no_prior_cov_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    fs::remove_file(crate::indexer::coverage::coverage_path(&out)).expect("drop the report");
    let mut args = full_args(&repo, &out);
    args["full"] = json!(false);
    do_index_codebase(&args).expect("incremental index");
    let m = crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(&out))
        .expect("manifest");
    assert!(m.scope.is_none(), "{:?}", m.scope);
}

#[test]
fn an_edit_between_the_change_check_and_the_refresh_rebuilds() {
    // Claude review of #478: the TOCTOU fallback in `try_reuse`. The reuse
    // check opens the graph (`graph_has_lsp_rows`) after the change check and
    // before the refresh; the release hook edits a file there.
    let (repo, base) = fixture("analyze443_toctou_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    EDIT_ON_RELEASE.with(|c| *c.borrow_mut() = Some(repo.join("src/b.rs")));
    crate::graph_store::register_release_hook(edit_on_first_release);
    let resp = analyze(&repo, &out, json!({}));
    assert!(
        EDIT_ON_RELEASE.with(|c| c.borrow().is_none()),
        "the edit ran during the call"
    );
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&resp), outcome(&fresh));
}

#[test]
fn an_incremental_pass_whose_coverage_write_fails_records_no_scope() {
    // The same rule on `index_codebase`'s incremental path: a manifest that
    // vouches for the graph must not sit beside another pass's report.
    let (repo, base) = fixture("analyze443_inc_covfail_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let blocker = crate::indexer::coverage::coverage_path(&out).with_extension("json.tmp");
    fs::create_dir_all(&blocker).expect("block the temp file");
    let mut args = full_args(&repo, &out);
    args["full"] = json!(false);
    do_index_codebase(&args).expect("index");
    let m = crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(&out))
        .expect("manifest");
    assert!(m.scope.is_none(), "{:?}", m.scope);
}

/// A crate whose test calls a method through `use <crate name>::Set`: which
/// receiver the call resolves to depends on the crate name in `Cargo.toml`,
/// a file the `language: "rust"` walk does not list (Codex review of #478).
fn cargo_fixture(prefix: &str, crate_name: &str) -> (PathBuf, TestTempDir) {
    let base = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let repo = base.join("repo");
    fs::create_dir_all(repo.join("src")).expect("mk src");
    fs::create_dir_all(repo.join("tests")).expect("mk tests");
    write_cargo_toml(&repo, crate_name);
    fs::write(
        repo.join("src/lib.rs"),
        "pub struct Set;\nimpl Set {\n    pub fn m(&self) {}\n}\n",
    )
    .expect("write lib.rs");
    fs::write(
        repo.join("tests/t.rs"),
        "use old_name::Set;\n\n#[test]\nfn f() {\n    let s: Set = Set;\n    s.m();\n}\n",
    )
    .expect("write t.rs");
    (repo, base)
}

/// Without `cargo` both passes record the same empty map, so the rebuild these
/// tests expect would not happen (Claude review of #478).
fn cargo_available() -> bool {
    std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn write_cargo_toml(repo: &Path, crate_name: &str) {
    fs::write(
        repo.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n"
        ),
    )
    .expect("write Cargo.toml");
}

#[test]
fn an_unchanged_cargo_crate_is_reused() {
    if !cargo_available() {
        return;
    }
    let (repo, base) = cargo_fixture("analyze443_cargo_same_", "old_name");
    let out = base.join("out");
    let first = analyze(&repo, &out, json!({}));
    let second = analyze(&repo, &out, json!({}));
    // The full and the incremental pass fingerprint the same Cargo map alike.
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert_eq!(outcome(&second), outcome(&first));
}

#[test]
fn an_edit_to_cargo_toml_alone_rebuilds() {
    if !cargo_available() {
        return;
    }
    let (repo, base) = cargo_fixture("analyze443_cargo_edit_", "old_name");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));

    write_cargo_toml(&repo, "new_name");
    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["index"]["mode"], "full", "{second}");

    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&second), outcome(&fresh));
}

#[test]
fn an_index_codebase_pass_that_sees_the_cargo_map_move_records_no_scope() {
    // Claude review of #478: the `index_codebase` route. Its pass refreshes the
    // facts, so the next analysis compares equal digests: only a dropped scope
    // makes it rebuild.
    if !cargo_available() {
        return;
    }
    let (repo, base) = cargo_fixture("analyze443_cargo_ix_", "old_name");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    write_cargo_toml(&repo, "new_name");
    let mut args = full_args(&repo, &out);
    args["full"] = json!(false);
    do_index_codebase(&args).expect("index");
    let m = crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(&out))
        .expect("manifest");
    assert!(m.scope.is_none(), "{:?}", m.scope);
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
fn a_manifest_without_a_cargo_digest_is_not_reused() {
    // A manifest written before the digest existed: unknown, never a match.
    let (repo, base) = fixture("analyze443_no_digest_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let path = crate::indexer::manifest::manifest_path(&out);
    let mut m: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
    assert!(m["cargo_facts"].is_string(), "precondition: {m}");
    m.as_object_mut().expect("object").remove("cargo_facts");
    fs::write(&path, m.to_string()).expect("write");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

thread_local! {
    /// The file `remove_on_first_release` removes, armed by the test.
    static REMOVE_ON_RELEASE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

fn remove_on_first_release(_path: &Path) -> Result<(), String> {
    if let Some(file) = REMOVE_ON_RELEASE.with(|c| c.borrow_mut().take()) {
        fs::remove_file(&file).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[test]
fn a_report_that_vanishes_before_the_refresh_is_not_reused() {
    // Claude review of #478: `reusable_manifest` loads the coverage sidecar,
    // then the refresh loads it again. Removed in between (a concurrent full
    // rebuild forgetting it), the refresh carries no gaps forward and drops
    // the scope; this call must rebuild too, not present that report.
    let (repo, base) = fixture("analyze443_cov_vanish_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let cov = crate::indexer::coverage::coverage_path(&out);
    REMOVE_ON_RELEASE.with(|c| *c.borrow_mut() = Some(cov.clone()));
    crate::graph_store::register_release_hook(remove_on_first_release);
    let resp = analyze(&repo, &out, json!({}));
    assert!(
        REMOVE_ON_RELEASE.with(|c| c.borrow().is_none()),
        "the report was removed during the call"
    );
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

fn files_with_a_target_context(graph: &Path) -> usize {
    let store = crate::graph_store::GraphStore::open_or_create(graph).expect("open graph");
    store
        .execute_query("MATCH (f:File) WHERE f.target_context <> '' RETURN f.id")
        .expect("query")
        .rows
        .len()
}

#[test]
fn an_unchanged_pass_rewrites_cargo_facts_a_failed_write_lost() {
    // Claude review of #478: every pass writes the Cargo facts again, so one
    // that a best-effort write lost is repaired by the next pass. Clearing a
    // column stands in for that failed write.
    if !cargo_available() {
        return;
    }
    let (repo, base) = cargo_fixture("analyze443_facts_repair_", "old_name");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let graph = out.join("graph");
    assert!(files_with_a_target_context(&graph) > 0, "precondition");
    {
        let store = crate::graph_store::GraphStore::open_or_create(&graph).expect("open graph");
        store
            .execute_query("MATCH (f:File) SET f.target_context = ''")
            .expect("clear");
    }
    assert_eq!(files_with_a_target_context(&graph), 0);
    let mut args = full_args(&repo, &out);
    args["full"] = json!(false);
    do_index_codebase(&args).expect("unchanged incremental pass");
    assert!(
        files_with_a_target_context(&graph) > 0,
        "the facts are back"
    );
}

#[test]
#[cfg(unix)]
fn a_coverage_sidecar_that_is_not_a_regular_file_is_rebuilt_not_read() {
    // Codex review of #478: the reuse check reads the coverage sidecar. A FIFO
    // with no writer would block that read forever; it must read as absent,
    // so the call rebuilds (and the rebuild removes it).
    let (repo, base) = fixture("analyze443_cov_fifo_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let cov = crate::indexer::coverage::coverage_path(&out);
    fs::remove_file(&cov).expect("drop the report");
    let made = std::process::Command::new("mkfifo")
        .arg(&cov)
        .status()
        .expect("mkfifo");
    assert!(made.success());
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
#[cfg(unix)]
fn a_meta_json_that_is_not_a_regular_file_is_rebuilt_not_read() {
    // Codex security review of #478: `same_root` reads `meta.json` before
    // reusing a graph. A FIFO with no writer would block that read forever.
    let (repo, base) = fixture("analyze443_meta_fifo_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let meta = out.join("meta.json");
    fs::remove_file(&meta).expect("drop meta.json");
    let made = std::process::Command::new("mkfifo")
        .arg(&meta)
        .status()
        .expect("mkfifo");
    assert!(made.success());
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}
