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
        // `expect`, not `if let Ok`: a failing query must not compare empty
        // with empty (review of #478).
        let qr = store
            .execute_query(&q)
            .unwrap_or_else(|e| panic!("{q}: {e}"));
        rows.extend(qr.rows.iter().map(|r| format!("{rel}|{}|{}", r[0], r[1])));
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
        "lsp": false,
    }))
    .expect_err("a coverage receipt that did not reach the sidecar must not be returned");
    // The reuse path's own refusal, not a full rebuild's: the full path would
    // have removed the manifest first (`forget_sidecars`).
    assert!(err.starts_with("coverage sidecar write failed"), "{err}");
    assert!(
        crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(&out)).is_some(),
        "the error came from the reuse path, which keeps the manifest"
    );
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
    assert!(
        !out.join("meta.json").exists(),
        "nor the previous meta.json"
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

#[test]
fn a_missing_or_unreadable_coverage_sidecar_is_not_reused() {
    // Codex review of #478: the refresh carries gaps forward from this report,
    // so without it a reused graph would report clean coverage.
    let (repo, base) = fixture("analyze443_nocov_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let cov = crate::indexer::coverage::coverage_path(&out);
    fs::write(&cov, b"{ not json").expect("corrupt sidecar");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
    fs::remove_file(&cov).expect("rm sidecar");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[cfg(unix)]
#[test]
fn a_symlinked_graph_is_not_reused_and_its_target_is_left_alone() {
    // Codex review of #478: reuse followed the link and the later phases
    // rewrote the other graph's communities.
    let (repo, base) = fixture("analyze443_symlink_");
    let out = base.join("out");
    let other = base.join("other");
    analyze(&repo, &out, json!({}));
    analyze(&repo, &other, json!({}));
    let before = edge_rows(&json!({ "graph_path": other.join("graph").to_string_lossy() }));

    let graph = out.join("graph");
    if graph.is_dir() {
        fs::remove_dir_all(&graph).expect("rm graph dir");
    } else {
        fs::remove_file(&graph).expect("rm graph file");
    }
    std::os::unix::fs::symlink(other.join("graph"), &graph).expect("symlink");

    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
    assert!(
        !fs::symlink_metadata(&graph)
            .expect("graph")
            .file_type()
            .is_symlink(),
        "the full path replaces the link with a graph of its own"
    );
    let after = edge_rows(&json!({ "graph_path": other.join("graph").to_string_lossy() }));
    assert_eq!(before, after, "the link target is untouched");
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
fn reuse_with_an_excluded_directory_keeps_the_files_indexed_count() {
    // Codex review of #478: the reuse path subtracted the excluded directory's
    // `skipped` entry from `files_indexed`.
    let (repo, base) = fixture("analyze443_count_");
    let out = base.join("out");
    let args = json!({ "exclude_dirs": ["extra"] });
    let first = analyze(&repo, &out, args.clone());
    let second = analyze(&repo, &out, args);
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert_eq!(
        second["coverage"]["files_indexed"], first["coverage"]["files_indexed"],
        "{second}"
    );
}

#[test]
fn another_checkout_with_matching_files_is_rebuilt() {
    // Review of #478: identical relative paths, sizes and mtimes (normalized
    // mtimes, `cp -p`) passed the change check for another root.
    let (repo, base) = fixture("analyze443_root_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let copy = base.join("copy");
    for entry in walk(&repo) {
        let rel = entry.strip_prefix(&repo).expect("rel");
        let dst = copy.join(rel);
        fs::create_dir_all(dst.parent().expect("parent")).expect("mkdir");
        fs::copy(&entry, &dst).expect("copy");
        let mtime = fs::metadata(&entry)
            .and_then(|m| m.modified())
            .expect("mtime");
        fs::File::options()
            .write(true)
            .open(&dst)
            .and_then(|f| f.set_modified(mtime))
            .expect("keep mtime");
    }
    let resp = analyze(&copy, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).expect("read_dir") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn a_graph_with_language_server_rows_is_not_reused() {
    // Review of #478: an earlier `lsp: true` run's rows survived reuse with
    // `lsp: false`, which a fresh analysis would not have.
    let (repo, base) = fixture("analyze443_lsp_");
    let out = base.join("out");
    let first = analyze(&repo, &out, json!({}));
    {
        let path = PathBuf::from(first["graph_path"].as_str().expect("graph_path"));
        let store = crate::graph_store::GraphStore::open_or_create(&path).expect("open");
        let mut marked = 0;
        for &(rel, from, to) in crate::graph_store::REL_TABLES {
            if !rel.starts_with("Calls_") || marked > 0 {
                continue;
            }
            let q = format!(
                "MATCH (:{from})-[r:{rel}]->(:{to}) SET r.resolution_method = 'lsp-definition' \
                 RETURN count(r)"
            );
            if let Ok(qr) = store.execute_query(&q) {
                marked += qr
                    .rows
                    .first()
                    .and_then(|r| r[0].parse::<u64>().ok())
                    .unwrap_or(0);
            }
        }
        assert!(marked > 0, "precondition: a call edge to mark");
    }
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
    // And `lsp: true` itself never reuses.
    let resp = analyze(&repo, &out, json!({ "lsp": true }));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
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
fn a_graph_imported_from_an_artifact_is_rebuilt_from_source() {
    // Claude review of #478: the bundled manifest carried the exporter's scope,
    // so analyze reused a graph it never built from source (a doctored
    // `graph.zst` would survive every later call).
    let (repo, base) = fixture("analyze443_artifact_");
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@ap.dev"]);
    git(&["config", "user.name", "AP"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "--no-gpg-sign", "-m", "initial"]);
    let index = |out: &Path, extra: Value| {
        let mut args = json!({
            "path": repo.to_string_lossy(),
            "output_dir": out.to_string_lossy(),
            "language": "rust",
        });
        for (k, v) in extra.as_object().expect("object") {
            args[k] = v.clone();
        }
        crate::indexing_handlers::do_index_codebase(&args).expect("index")
    };
    index(&base.join("exporter"), json!({ "export_artifact": true }));

    let out = base.join("out");
    let imported = index(&out, json!({ "bootstrap": true }));
    assert_eq!(imported["source"], "artifact_bootstrap", "{imported}");
    let m = crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(&out))
        .expect("bundled manifest");
    assert_eq!(
        m.scope, None,
        "an imported manifest must not vouch for the graph"
    );

    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
}

#[test]
fn reuse_survives_its_own_refresh() {
    // Review of #478: a refresh that dropped the scope would make only the
    // second call reuse; the third must too.
    let (repo, base) = fixture("analyze443_thrice_");
    let out = base.join("out");
    let first = analyze(&repo, &out, json!({}));
    for call in ["second", "third"] {
        let resp = analyze(&repo, &out, json!({}));
        assert_eq!(resp["index"]["mode"], "incremental", "{call}: {resp}");
        assert_eq!(outcome(&resp), outcome(&first), "{call}");
    }
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
    // The production ordering itself is pinned by
    // `prepare_full_rebuild_snapshots_before_the_index_runs` below.
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

fn graph_sidecars_intact(out: &Path) {
    assert!(
        crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(out)).is_some(),
        "manifest kept"
    );
    assert!(out.join("meta.json").exists(), "meta.json kept");
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

fn refuse_release(_path: &Path) -> Result<(), String> {
    Err("graph_handle_in_use: held by a running request (test)".into())
}

#[test]
fn a_busy_graph_keeps_its_sidecars_when_the_rebuild_is_refused() {
    // Review of #478: handles are released before the sidecars are forgotten,
    // so a busy graph fails while its manifest and meta.json still describe
    // it. The hook is per thread; each test runs on its own.
    let (repo, base) = fixture("analyze443_busy_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    crate::graph_store::register_release_hook(refuse_release);
    let err = do_analyze_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "language": "rust",
        "full": true,
    }))
    .expect_err("the graph is busy");
    assert!(err.contains("graph_handle_in_use"), "{err}");
    graph_sidecars_intact(&out);
}

#[test]
fn prepare_full_rebuild_snapshots_before_the_index_runs() {
    // Review of #478: drive the production pieces in the order both full
    // paths use them. A file edited after `prepare_full_rebuild` (that is,
    // during the index) must make the next analysis rebuild.
    let (repo, base) = fixture("analyze443_prod_order_");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let opts = crate::indexer::IndexOptions {
        language_filter: Some(crate::parser::Language::Rust),
        ..Default::default()
    };
    let graph = out.join("graph");
    let snapshot = crate::query_handlers::prepare_full_rebuild(&repo, &out, &graph, &opts)
        .expect("prepare")
        .expect("snapshot");
    let b = repo.join("src/b.rs");
    fs::write(&b, "pub fn beta() { edited(); }\n").expect("edit during the index");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    fs::File::options()
        .write(true)
        .open(&b)
        .and_then(|f| f.set_modified(later))
        .expect("bump mtime");
    let result = crate::indexer::index_codebase_with_language(&repo, &graph, &opts).expect("index");
    crate::indexer::coverage::save(
        &crate::indexer::coverage::coverage_path(&out),
        &result.coverage,
    )
    .expect("coverage");
    crate::indexer::manifest::save(&crate::indexer::manifest::manifest_path(&out), &snapshot)
        .expect("save the pre-index snapshot");
    crate::query_handlers::write_graph_meta(&out, &repo).expect("meta");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "full", "{resp}");
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
    let (repo, base) = cargo_fixture("analyze443_cargo_edit_", "old_name");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));

    write_cargo_toml(&repo, "new_name");
    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["index"]["mode"], "full", "{second}");

    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(outcome(&second), outcome(&fresh));
}
