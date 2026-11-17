//! Issue #443: past `MAX_TOUCHED_PCT` of the tree changed, analyze rebuilds
//! instead of refreshing, and the declined refresh leaves the previous state
//! as it was: the manifest, the coverage sidecar, every node and every
//! relationship row with its properties (the schema DDL that runs before the
//! decline aside). Driven through `do_analyze_codebase` with the bound in
//! force (the shared `analyze` helper lifts it).

use super::tests::{edge_rows_with_properties, node_rows, outcome};
use super::MAX_TOUCHED_PCT;
use crate::analyze_handlers::do_analyze_codebase;
use crate::test_support::{TempDirExt, TestTempDir};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Ten files: one edit is 10 % of the tree, so the bound falls between two
/// whole numbers of edited files.
const FILES: usize = 10;

/// Each module function calls its predecessor, so the graph holds `Calls_*`
/// rows the resolver wrote before any edit (review of #556); `edit` moves
/// them to the successor.
fn seeded_body(i: usize) -> String {
    let prev = (i + FILES - 1) % FILES;
    format!("pub fn f{i}() {{ crate::m{prev}::f{prev}(); }}\n")
}

fn tree(prefix: &str) -> (PathBuf, TestTempDir) {
    let base = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("temp dir")
        .keep_managed();
    let src = base.join("repo/src");
    fs::create_dir_all(&src).expect("mk src");
    let mods: String = (0..FILES).map(|i| format!("mod m{i};\n")).collect();
    fs::write(src.join("lib.rs"), mods).expect("lib.rs");
    for i in 0..FILES {
        fs::write(src.join(format!("m{i}.rs")), seeded_body(i)).expect("m");
    }
    (base.join("repo"), base)
}

fn run(repo: &Path, out: &Path, full: bool) -> Value {
    let resp = do_analyze_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "language": "rust",
        "full": full,
    }))
    .expect("analyze");
    assert_eq!(resp["status"], "ok", "{resp}");
    resp
}

/// Edits `k` module files, each now calling its successor instead of its
/// predecessor, so the edit moves edges.
fn edit(repo: &Path, k: usize) {
    for i in 0..k {
        let next = (i + 1) % FILES;
        let body = format!("pub fn f{i}() {{ crate::m{next}::f{next}(); }}\n");
        fs::write(repo.join(format!("src/m{i}.rs")), body).expect("edit");
    }
}

/// The largest number of the tree's files (`FILES` modules plus `lib.rs`) the
/// bound still refreshes.
fn at_bound() -> usize {
    (FILES + 1) * MAX_TOUCHED_PCT as usize / 100
}

#[test]
fn a_change_set_at_the_bound_is_refreshed() {
    let (repo, base) = tree("analyze443_bound_at_");
    let out = base.join("out");
    run(&repo, &out, false);
    edit(&repo, at_bound());
    let resp = run(&repo, &out, false);
    assert_eq!(resp["index"]["mode"], "incremental", "{resp}");
}

#[test]
fn a_change_set_over_the_bound_is_rebuilt_like_a_fresh_analysis() {
    let (repo, base) = tree("analyze443_bound_over_");
    let out = base.join("out");
    run(&repo, &out, false);
    edit(&repo, at_bound() + 1);
    let resp = run(&repo, &out, false);
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
    let fresh = run(&repo, &base.join("fresh"), true);
    assert_eq!(outcome(&resp), outcome(&fresh));
}

#[test]
fn a_declined_refresh_leaves_the_manifest_and_the_graph_rows_as_they_were() {
    let (repo, base) = tree("analyze443_bound_declined_");
    let out = base.join("out");
    let first = run(&repo, &out, false);
    let manifest = crate::indexer::manifest::manifest_path(&out);
    let before = fs::read(&manifest).expect("manifest");
    let sidecar = crate::indexer::coverage::coverage_path(&out);
    let coverage_before = fs::read(&sidecar).expect("coverage sidecar");
    let nodes_before = node_rows(&first);
    let properties_before = edge_rows_with_properties(&first);
    edit(&repo, at_bound() + 1);
    let prior = crate::indexer::manifest::load(&manifest).expect("load");
    let refresh = crate::indexer::index_incremental_bounded(
        &repo,
        &out.join("graph"),
        &manifest,
        &crate::indexer::IndexOptions {
            language_filter: Some(crate::parser::Language::Rust),
            ..Default::default()
        },
        &prior,
        MAX_TOUCHED_PCT,
    )
    .expect("bounded refresh");
    let crate::indexer::Refresh::Declined { touched, files } = refresh else {
        panic!("over the bound, the refresh must decline");
    };
    assert_eq!(
        (touched, files),
        ((at_bound() + 1) as u64, (FILES + 1) as u64)
    );
    assert_eq!(
        fs::read(&manifest).expect("manifest"),
        before,
        "manifest untouched"
    );
    assert_eq!(
        fs::read(&sidecar).expect("coverage sidecar"),
        coverage_before,
        "coverage sidecar untouched"
    );
    // The graph on disk is read again: every node, and the resolution flag of
    // the call sites and imports, the only nodes `node_rows` reads it for
    // (review of #552, wording of #558), with `node_rows` alone rather than
    // the whole `outcome` (review of #557). The relationship rows are compared below with their
    // properties, which implies their endpoints (review of #556).
    assert_eq!(node_rows(&first), nodes_before, "nodes untouched");
    // The properties of every row, `confidence` and `resolution_method`
    // among them (review of #555), including the `Calls_*` rows the resolver
    // wrote from the seeded calls, not only the constant `Defines_*` rows
    // (review of #556).
    let properties_after = edge_rows_with_properties(&first);
    // One resolver-written row pinned with its value: every `Calls_*` row has
    // a `|resolution_method=` field, null included, so its presence alone
    // proved only that a call row exists (review of #557). The value is the
    // policy's for an import match, not a copy of it, so a deliberate change
    // of the policy moves the pin with it (review of #558).
    let evidence = crate::ambiguity_policy::Evidence::ImportMatch;
    // The trailing `|` and `starts_with` rely on `observed_count` following
    // `resolution_method` in the table's columns: the row goes on after the
    // pin, so an equality here would never hold (review of #558).
    let pinned = format!(
        "Calls_Function_Function|src/m1.rs::f1|src/m0.rs::f0\
         |confidence={}|resolution_method={}|",
        crate::ambiguity_policy::confidence_for(evidence),
        crate::ambiguity_policy::resolution_label(evidence),
    );
    assert!(
        properties_before.iter().any(|r| r.starts_with(&pinned)),
        "the rows compared include the resolver-written call m1::f1 -> m0::f0 with its method: {properties_before:?}"
    );
    assert_eq!(
        properties_after, properties_before,
        "edge rows and their properties untouched"
    );
}
