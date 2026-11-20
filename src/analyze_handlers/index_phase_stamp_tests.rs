//! Issue #443: a second `analyze_codebase` on an unchanged tree skips
//! resolve, cluster and the search index, and repeats their blocks, only when
//! the graph and the search index are the ones the previous analysis left
//! (`analysis_stamp`). Every other case runs every phase.

use super::cargo_tests::{cargo_available, cargo_fixture, write_cargo_toml};
use super::tests::{analyze, edge_rows_with_properties, fixture, node_rows, outcome};
use crate::analyze_handlers::analysis_stamp::STAMP_FILE;
use crate::graph_store::GraphStore;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

fn later_blocks(resp: &Value) -> Value {
    json!([resp["resolve"], resp["cluster"], resp["search_index"]])
}

fn graph_of(resp: &Value) -> PathBuf {
    PathBuf::from(resp["graph_path"].as_str().expect("graph_path"))
}

#[test]
fn an_unchanged_second_call_skips_the_later_phases_and_matches_a_fresh_analysis() {
    let (repo, base) = fixture("ap_stamp_same_");
    let out = base.join("out");
    let first = analyze(&repo, &out, json!({}));
    assert_eq!(first["phases_reused"], false, "{first}");
    assert!(
        out.join(STAMP_FILE).is_file(),
        "a completed analysis leaves a stamp"
    );

    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert_eq!(second["phases_reused"], true, "{second}");
    assert_eq!(later_blocks(&second), later_blocks(&first));
    // A third call skips again: the stamp was written anew after the second.
    let third = analyze(&repo, &out, json!({}));
    assert_eq!(third["phases_reused"], true, "{third}");

    // The graph the skip leaves is the one a from-scratch analysis builds.
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&third), outcome(&fresh));
    assert_eq!(
        edge_rows_with_properties(&third),
        edge_rows_with_properties(&fresh)
    );
    assert_eq!(node_rows(&third), node_rows(&fresh));
}

#[test]
fn an_edited_file_runs_every_phase() {
    let (repo, base) = fixture("ap_stamp_edit_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    fs::write(
        repo.join("src/b.rs"),
        "pub fn beta() {}\npub fn gamma() {}\npub fn delta() { gamma(); }\n",
    )
    .expect("edit b.rs");
    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["index"]["changed"], 1, "{second}");
    assert_eq!(second["phases_reused"], false, "{second}");
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&second), outcome(&fresh));
}

#[test]
fn another_resolution_or_full_true_runs_every_phase() {
    let (repo, base) = fixture("ap_stamp_gamma_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let other = analyze(&repo, &out, json!({ "resolution_param": 0.5 }));
    assert_eq!(other["phases_reused"], false, "{other}");
    // The stamp now records 0.5, so the default resolution runs again too.
    let back = analyze(&repo, &out, json!({}));
    assert_eq!(back["phases_reused"], false, "{back}");
    let full = analyze(&repo, &out, json!({ "full": true }));
    assert_eq!(full["index"]["mode"], "full", "{full}");
    assert_eq!(full["phases_reused"], false, "{full}");
}

/// Any other writer between two analyses changes a file of the graph or of
/// the search index, and its rows must not be reported under the old counts.
#[test]
fn a_graph_written_since_the_last_analysis_runs_every_phase() {
    let (repo, base) = fixture("ap_stamp_graph_");
    let out = base.join("out");
    let first = analyze(&repo, &out, json!({}));
    {
        let store = GraphStore::open_or_create(&graph_of(&first)).expect("open graph");
        store
            .execute_query("CREATE (:Function {id: 'injected', name: 'injected'})")
            .expect("write a node");
    }
    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["phases_reused"], false, "{second}");
}

#[test]
fn a_search_index_changed_since_the_last_analysis_runs_every_phase() {
    let (repo, base) = fixture("ap_stamp_search_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let stray = out.join(crate::search::SEARCH_INDEX_DIR).join("stray");
    fs::write(&stray, b"x").expect("write into the search index");
    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["phases_reused"], false, "{second}");
}

#[test]
fn a_stamp_of_another_version_or_unreadable_is_not_used_and_is_replaced() {
    let (repo, base) = fixture("ap_stamp_version_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let path = out.join(STAMP_FILE);
    let mut stamp: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
    stamp["version"] = json!("0.0.0-other");
    fs::write(&path, serde_json::to_vec(&stamp).expect("ser")).expect("write");
    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["phases_reused"], false, "{second}");
    let rewritten: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
    assert_eq!(rewritten["version"], env!("CARGO_PKG_VERSION"));

    fs::write(&path, b"{not json").expect("corrupt");
    let third = analyze(&repo, &out, json!({}));
    assert_eq!(third["phases_reused"], false, "{third}");
    assert_eq!(analyze(&repo, &out, json!({}))["phases_reused"], true);
}

/// A call that fails after phase 1 started leaves no stamp: `take` removes it
/// before anything is written.
#[test]
fn a_call_that_fails_leaves_no_stamp() {
    let (repo, base) = fixture("ap_stamp_fail_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    assert!(out.join(STAMP_FILE).is_file());
    // Block the coverage sidecar's temporary file: the refresh then fails the
    // call after it re-indexed (`a_coverage_write_that_fails_on_reuse_fails_the_analysis`).
    let cov = crate::indexer::coverage::coverage_path(&out);
    fs::create_dir_all(cov.with_extension("json.tmp")).expect("block the temp file");
    let err = crate::analyze_handlers::do_analyze_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "language": "rust",
    }))
    .expect_err("the coverage write fails the call");
    assert!(err.starts_with("coverage sidecar write failed"), "{err}");
    assert!(
        !out.join(STAMP_FILE).exists(),
        "a failed call leaves no stamp"
    );
}

/// The Cargo facts are written again by every pass and read by the resolver:
/// a `Cargo.toml` the `language: "rust"` walk does not list can move them
/// while no listed file changes.
#[test]
fn a_moved_cargo_map_runs_every_phase() {
    if !cargo_available() {
        eprintln!("cargo not on PATH; the Cargo map cannot move");
        return;
    }
    let (repo, base) = cargo_fixture("ap_stamp_cargo_", "old_name");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    assert_eq!(analyze(&repo, &out, json!({}))["phases_reused"], true);
    write_cargo_toml(
        &repo,
        "old_name",
        "[features]\ndefault = [\"x\"]\nx = []\n\n",
    );
    let moved = analyze(&repo, &out, json!({}));
    assert_eq!(moved["index"]["mode"], "incremental", "{moved}");
    assert_eq!(moved["index"]["changed"], 0, "{moved}");
    assert_eq!(moved["phases_reused"], false, "{moved}");
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&moved), outcome(&fresh));
}
