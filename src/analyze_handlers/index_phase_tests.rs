//! Issue #443: a second `analyze_codebase` on the same `output_dir` reuses the
//! graph incrementally, and the graph it leaves equals a from-scratch one.

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
        "graph": resp["graph"],
        "resolve_edges": resp["resolve"]["total_edges"],
        "communities": resp["cluster"]["community_count"],
        "processes": resp["cluster"]["process_count"],
        "bm25_docs": resp["search_index"]["bm25_doc_count"],
    })
}

#[test]
fn a_second_call_on_an_unchanged_tree_reparses_nothing() {
    let (repo, base) = fixture("analyze443_unchanged_");
    let out = base.join("out");
    let first = analyze(&repo, &out, json!({}));
    assert_eq!(first["index"]["mode"], "full", "{first}");

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
fn an_incremental_call_after_an_edit_matches_a_full_rebuild() {
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

    let incremental = analyze(&repo, &out, json!({}));
    assert_eq!(incremental["index"]["mode"], "incremental", "{incremental}");
    assert_eq!(incremental["index"]["changed"], 2, "{incremental}");
    assert_eq!(incremental["index"]["added"], 1, "{incremental}");
    assert_eq!(incremental["index"]["deleted"], 1, "{incremental}");

    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(fresh["index"]["mode"], "full");
    assert_eq!(outcome(&incremental), outcome(&fresh));
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
