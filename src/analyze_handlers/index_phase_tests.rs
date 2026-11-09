//! Issue #443: a second `analyze_codebase` on the same `output_dir` reuses the
//! graph when nothing changed, rebuilds it otherwise, and either way leaves
//! the graph a from-scratch analysis would.

use crate::analyze_handlers::do_analyze_codebase;
use crate::test_support::{TempDirExt, TestTempDir};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// A three-file crate with calls across files, so resolve and cluster have
/// real work, and an `extra/` directory an `exclude_dirs` change can drop
/// (not `vendor/`, which the walk already prunes by default).
fn fixture(prefix: &str) -> (PathBuf, TestTempDir) {
    let base = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = base.join("repo/src");
    fs::create_dir_all(src.join("extra")).expect("mk repo");
    fs::write(
        src.join("lib.rs"),
        "mod a;\nmod b;\npub fn root() { a::alpha(); }\n",
    )
    .expect("write lib.rs");
    fs::write(src.join("a.rs"), "pub fn alpha() { crate::b::beta(); }\n").expect("write a.rs");
    fs::write(
        src.join("b.rs"),
        "pub fn beta() {}\npub fn gamma() { beta(); }\n",
    )
    .expect("write b.rs");
    fs::write(src.join("extra/v.rs"), "pub fn vendored() {}\n").expect("write v.rs");
    (base.join("repo"), base)
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
    let resp = do_analyze_codebase(&args).expect("analyze");
    assert_eq!(resp["status"], "ok", "{resp}");
    resp
}

/// Everything a caller reads off a completed analysis that must not depend on
/// which index path produced the graph.
fn outcome(resp: &Value) -> Value {
    json!({
        "edges": edge_rows(resp),
        "graph": resp["graph"],
        "resolve_edges": resp["resolve"]["total_edges"],
        "communities": resp["cluster"]["community_count"],
        "processes": resp["cluster"]["process_count"],
        "bm25_docs": resp["search_index"]["bm25_doc_count"],
    })
}

/// Every relationship row of the graph, as sorted `table|from|to` strings, so
/// two graphs with equal totals but a different call target still differ.
fn edge_rows(resp: &Value) -> Vec<String> {
    let path = PathBuf::from(resp["graph_path"].as_str().expect("graph_path"));
    let store = crate::graph_store::GraphStore::open_or_create(&path).expect("open graph");
    let mut rows = Vec::new();
    for &(rel, from, to) in crate::graph_store::REL_TABLES {
        let q = format!("MATCH (a:{from})-[r:{rel}]->(b:{to}) RETURN a.id, b.id");
        if let Ok(qr) = store.execute_query(&q) {
            rows.extend(qr.rows.iter().map(|r| format!("{rel}|{}|{}", r[0], r[1])));
        }
    }
    rows.sort();
    rows
}

#[test]
fn a_second_call_on_an_unchanged_tree_reparses_nothing() {
    let (repo, base) = fixture("analyze443_unchanged_");
    let out = base.join("out");
    let first = analyze(&repo, &out, json!({}));
    assert_eq!(first["index"]["mode"], "full", "{first}");
    assert!(
        edge_rows(&first).iter().any(|r| r.starts_with("Calls_")),
        "the comparison must see the call edges"
    );

    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert_eq!(second["index"]["files_reparsed"], 0, "{second}");
    assert_eq!(second["index"]["changed"], 0, "{second}");
    assert_eq!(
        second["index"]["unchanged"], first["index"]["files_indexed"],
        "{second}"
    );
    assert_eq!(outcome(&second), outcome(&first));
    // The coverage receipt is the same apart from naming the path that ran.
    let mut coverage = second["coverage"].clone();
    assert_eq!(coverage["index_mode"], "incremental");
    coverage["index_mode"] = first["coverage"]["index_mode"].clone();
    assert_eq!(coverage, first["coverage"]);
}

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
fn a_changed_setting_rebuilds_in_full() {
    let (repo, base) = fixture("analyze443_settings_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));

    let excluded = analyze(&repo, &out, json!({ "exclude_dirs": ["extra"] }));
    assert_eq!(excluded["index"]["mode"], "full", "{excluded}");
    let fresh = analyze(
        &repo,
        &base.join("fresh"),
        json!({ "exclude_dirs": ["extra"] }),
    );
    assert_eq!(outcome(&excluded), outcome(&fresh));

    // Same settings again: reusable; the exclusion is now the recorded one.
    let again = analyze(&repo, &out, json!({ "exclude_dirs": ["extra"] }));
    assert_eq!(again["index"]["mode"], "incremental", "{again}");
    // Back to no exclusion: rebuild, the excluded file returns.
    let back = analyze(&repo, &out, json!({}));
    assert_eq!(back["index"]["mode"], "full", "{back}");
}

#[test]
fn full_true_rebuilds_even_when_the_graph_is_reusable() {
    let (repo, base) = fixture("analyze443_full_");
    let out = base.join("out");
    let first = analyze(&repo, &out, json!({}));
    let forced = analyze(&repo, &out, json!({ "full": true }));
    assert_eq!(forced["index"]["mode"], "full", "{forced}");
    assert_eq!(outcome(&forced), outcome(&first));
}

#[test]
fn a_manifest_that_records_no_settings_is_not_trusted() {
    // A manifest written before #443 has no `scope`: unknown, not "matching".
    let (repo, base) = fixture("analyze443_legacy_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let path = crate::indexer::manifest::manifest_path(&out);
    let mut m: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("parse manifest");
    assert!(
        m.get("scope").is_some(),
        "a full analysis records its settings: {m}"
    );
    m.as_object_mut().expect("object").remove("scope");
    fs::write(&path, m.to_string()).expect("write legacy manifest");

    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
fn an_incremental_index_under_other_settings_forgets_the_recorded_ones() {
    // `index_codebase` takes no settings check: its incremental pass under a
    // new exclusion leaves a graph that matches neither setting, so the
    // manifest must stop vouching for either.
    let (repo, base) = fixture("analyze443_index_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    crate::indexing_handlers::do_index_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "language": "rust",
        "exclude_dirs": ["extra"],
    }))
    .expect("index");
    let m = crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(&out))
        .expect("manifest");
    assert_eq!(m.scope, None);
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
fn exclusions_compare_by_content_not_by_spelling_or_order() {
    use crate::indexer::ExcludeSet;
    let a = ExcludeSet::new(&["vendor".into(), "./config/secrets".into(), "gen".into()]);
    let b = ExcludeSet::new(&["gen".into(), "config//secrets/".into(), "vendor".into()]);
    assert_eq!(a.canonical_entries(), b.canonical_entries());
    // A root-level path entry and a bare name exclude different things.
    let path = ExcludeSet::new(&["./vendor".into()]);
    let name = ExcludeSet::new(&["vendor".into()]);
    assert_ne!(path.canonical_entries(), name.canonical_entries());
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
fn a_coverage_write_that_fails_on_reuse_fails_the_analysis() {
    // Codex review of #478, P2: the reuse path read the sidecar back, so a
    // failed write left the previous report on disk and presented it as this
    // call's. Block the temporary file the save writes first; the previous
    // sidecar stays readable.
    let (repo, base) = fixture("analyze443_covfail_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let cov = crate::indexer::coverage::coverage_path(&out);
    assert!(
        crate::indexer::coverage::load(&cov).is_some(),
        "precondition"
    );
    fs::create_dir_all(cov.with_extension("json.tmp")).expect("block the temp file");

    let err = do_analyze_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "language": "rust",
    }))
    .expect_err("a coverage receipt that did not reach the sidecar must not be returned");
    assert!(err.contains("coverage"), "{err}");
}

#[test]
fn a_graph_built_by_another_version_is_rebuilt() {
    let (repo, base) = fixture("analyze443_version_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let path = crate::indexer::manifest::manifest_path(&out);
    let mut m: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
    m["scope"]["indexer_version"] = json!("0.0.0-older");
    fs::write(&path, m.to_string()).expect("write");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
fn a_full_rebuild_that_fails_leaves_no_manifest_to_trust() {
    // Claude review of #478: a rebuild that failed before its sidecars were
    // written left the previous manifest, so a later call with the previous
    // settings reused a graph built under other ones (here: with `extra/`
    // indexed although the caller excludes it).
    let (repo, base) = fixture("analyze443_stale_manifest_");
    let out = base.join("out");
    analyze(&repo, &out, json!({ "exclude_dirs": ["extra"] }));
    let cov = crate::indexer::coverage::coverage_path(&out);
    fs::create_dir_all(cov.with_extension("json.tmp")).expect("block the temp file");
    do_analyze_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "language": "rust",
    }))
    .expect_err("the rebuild without the exclusion fails on its coverage write");
    assert!(
        crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(&out)).is_none(),
        "the previous manifest must not survive a failed rebuild"
    );
    fs::remove_dir_all(cov.with_extension("json.tmp")).expect("unblock");

    let resp = analyze(&repo, &out, json!({ "exclude_dirs": ["extra"] }));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
    let fresh = analyze(
        &repo,
        &base.join("fresh"),
        json!({ "exclude_dirs": ["extra"] }),
    );
    assert_eq!(outcome(&resp), outcome(&fresh));
}

#[test]
fn a_changed_language_or_dependency_scope_rebuilds() {
    let (repo, base) = fixture("analyze443_lang_scope_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let auto = analyze(&repo, &out, json!({ "language": "auto" }));
    assert_eq!(auto["index"]["mode"], "full", "{auto}");
    let scoped = analyze(
        &repo,
        &out,
        json!({ "language": "auto", "dependency_scope": "full" }),
    );
    assert_eq!(scoped["index"]["mode"], "full", "{scoped}");
    // The deprecated spelling of the same tier is the same setting.
    let legacy = analyze(
        &repo,
        &out,
        json!({ "language": "auto", "include_dependencies": true }),
    );
    assert_eq!(legacy["index"]["mode"], "incremental", "{legacy}");
}

#[test]
fn a_graph_the_refresh_cannot_open_is_rebuilt() {
    let (repo, base) = fixture("analyze443_corrupt_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let graph = out.join("graph");
    if graph.is_dir() {
        fs::remove_dir_all(&graph).expect("rm graph dir");
    } else {
        fs::remove_file(&graph).expect("rm graph file");
    }
    fs::write(&graph, b"not a database").expect("corrupt graph");

    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&resp), outcome(&fresh));
}
