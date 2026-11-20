//! Issue #443: an `analyze_codebase` whose refresh touched nothing reuses the
//! previous analysis's resolve, cluster and search-index results, and only
//! then; every case that must run them again is checked here.

use crate::analyze_handlers::do_analyze_codebase;
use crate::graph_store::seal::seal_path;
use crate::test_support::{TempDirExt, TestTempDir};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Calls across files, so resolve and cluster have real work.
fn fixture() -> (PathBuf, PathBuf, TestTempDir) {
    let base = tempfile::Builder::new()
        .prefix("ap-seal-")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = base.join("repo/src");
    fs::create_dir_all(&src).expect("mk repo");
    fs::write(
        base.join("repo/Cargo.toml"),
        "[package]\nname = \"seal\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(
        src.join("lib.rs"),
        "mod a;\npub fn root() { a::alpha(); }\n",
    )
    .expect("lib.rs");
    fs::write(
        src.join("a.rs"),
        "pub fn alpha() { beta(); }\npub fn beta() {}\n",
    )
    .expect("a.rs");
    (base.join("repo"), base.join("out"), base)
}

fn analyze(repo: &Path, out: &Path, extra: Value) -> Value {
    let mut args = json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "language": "rust",
    });
    for (k, v) in extra.as_object().expect("object") {
        args[k] = v.clone();
    }
    // A one-file edit is over the touched-share bound on a two-file tree.
    super::super::index_phase::TOUCHED_PCT_LIFTED.with(|c| c.set(true));
    let resp = do_analyze_codebase(&args).expect("analyze");
    assert_eq!(resp["status"], "ok", "{resp}");
    resp
}

fn reused(resp: &Value) -> bool {
    let flags: Vec<bool> = ["resolve", "cluster", "search_index"]
        .iter()
        .map(|k| resp[k]["reused"] == json!(true))
        .collect();
    assert!(
        flags.iter().all(|f| *f == flags[0]),
        "the three phases are reused together or not at all: {resp}"
    );
    flags[0]
}

/// The counts a caller reads, without the reuse flag and the timing.
fn counts(resp: &Value) -> Value {
    let mut v = json!({
        "resolve": resp["resolve"],
        "cluster": resp["cluster"],
        "bm25": resp["search_index"]["bm25_doc_count"],
        "vectors": resp["search_index"]["vector_doc_count"],
        "graph": resp["graph"],
    });
    for k in ["resolve", "cluster"] {
        v[k].as_object_mut().expect("block").remove("reused");
    }
    v
}

#[test]
fn an_unchanged_second_call_reuses_the_derived_phases() {
    let (repo, out, _keep) = fixture();
    let first = analyze(&repo, &out, json!({}));
    assert!(!reused(&first));
    assert_eq!(first["index"]["mode"], "full");
    assert!(
        seal_path(&out.join("graph")).is_file(),
        "a finished analysis seals"
    );
    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["index"]["mode"], "incremental");
    assert!(reused(&second), "{second}");
    assert_eq!(counts(&second), counts(&first));
    // The reuse seals again, so a third call reuses too.
    assert!(reused(&analyze(&repo, &out, json!({}))));
}

#[test]
fn an_edit_runs_the_phases_again() {
    let (repo, out, _keep) = fixture();
    analyze(&repo, &out, json!({}));
    fs::write(
        repo.join("src/a.rs"),
        "pub fn alpha() { beta(); gamma(); }\npub fn beta() {}\npub fn gamma() {}\n",
    )
    .expect("edit");
    let edited = analyze(&repo, &out, json!({}));
    assert_eq!(edited["index"]["changed"], 1, "{edited}");
    assert!(!reused(&edited));
    let fresh = analyze(&repo, &out.with_file_name("fresh"), json!({}));
    assert_eq!(counts(&edited)["resolve"], counts(&fresh)["resolve"]);
}

#[test]
fn another_tool_opening_the_graph_for_writing_withdraws_the_seal() {
    let (repo, out, _keep) = fixture();
    analyze(&repo, &out, json!({}));
    let resp = crate::symbol_handlers::run_resolve_graph(&json!({
        "graph_path": out.join("graph").to_string_lossy(),
    }));
    assert_ne!(resp["status"], "error", "{resp}");
    assert!(!seal_path(&out.join("graph")).exists());
    assert!(!reused(&analyze(&repo, &out, json!({}))));
}

#[test]
fn another_resolution_param_clusters_again() {
    let (repo, out, _keep) = fixture();
    analyze(&repo, &out, json!({}));
    let resp = analyze(&repo, &out, json!({ "resolution_param": 0.5 }));
    assert!(!reused(&resp));
    // And the seal now names 0.5: the default runs again too.
    assert!(!reused(&analyze(&repo, &out, json!({}))));
}

#[test]
fn a_full_call_never_reuses_and_the_next_one_does() {
    let (repo, out, _keep) = fixture();
    analyze(&repo, &out, json!({}));
    assert!(!reused(&analyze(&repo, &out, json!({ "full": true }))));
    assert!(reused(&analyze(&repo, &out, json!({}))));
}

#[test]
fn a_cargo_toml_edit_the_walk_does_not_list_resolves_again() {
    let (repo, out, _keep) = fixture();
    analyze(&repo, &out, json!({}));
    // `language: "rust"` walks `.rs` files only, so the refresh touches no
    // file; the Cargo facts move (another crate name).
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"renamed\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("edit Cargo.toml");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "incremental", "{resp}");
    assert_eq!(resp["index"]["changed"], 0, "{resp}");
    assert!(!reused(&resp), "{resp}");
}

#[test]
fn a_seal_naming_another_manifest_is_not_used() {
    let (repo, out, _keep) = fixture();
    analyze(&repo, &out, json!({}));
    let path = seal_path(&out.join("graph"));
    let mut seal: Value = serde_json::from_slice(&fs::read(&path).expect("seal")).expect("json");
    seal["manifest_sha256"] = json!("0".repeat(64));
    fs::write(&path, seal.to_string()).expect("rewrite seal");
    assert!(!reused(&analyze(&repo, &out, json!({}))));
}

#[test]
fn a_seal_that_is_not_json_is_not_used() {
    let (repo, out, _keep) = fixture();
    analyze(&repo, &out, json!({}));
    fs::write(seal_path(&out.join("graph")), b"{ torn").expect("tear seal");
    assert!(!reused(&analyze(&repo, &out, json!({}))));
}
