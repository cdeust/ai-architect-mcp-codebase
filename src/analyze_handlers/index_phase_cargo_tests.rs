//! Issue #443, Claude review of #523: a moved Cargo map (a `Cargo.toml` edit,
//! or an added, deleted or renamed `.rs` file, each a key of the map) is
//! applied to the previous graph, and the refreshed graph is the one a fresh
//! analysis builds: its rows, its nodes, the Cargo columns of every file and
//! `#[cfg]` twin, the crate evidence the resolver reads, and the coverage
//! receipt. Until this, the refresh dropped the manifest's scope and
//! `analyze_codebase` rebuilt every file after the incremental pass had run.

use super::rebuild_tests::full_args;
use super::tests::{analyze, outcome};
use crate::graph_store::{GraphStore, CFG_GATE_LABELS};
use crate::indexing_handlers::do_index_codebase;
use crate::test_support::{TempDirExt, TestTempDir};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// A crate whose test calls a method through `use <crate name>::Set`: which
/// receiver the call resolves to depends on the crate name in `Cargo.toml`,
/// a file the `language: "rust"` walk does not list (Codex review of #478).
/// `lib.rs` also holds a pair of `#[cfg(feature = "x")]` twins and a caller,
/// so which twin the call reaches depends on the default features.
fn cargo_fixture(prefix: &str, crate_name: &str) -> (PathBuf, TestTempDir) {
    let base = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let repo = base.join("repo");
    fs::create_dir_all(repo.join("src")).expect("mk src");
    fs::create_dir_all(repo.join("tests")).expect("mk tests");
    write_cargo_toml(&repo, crate_name, "");
    fs::write(
        repo.join("src/lib.rs"),
        "pub struct Set;\nimpl Set {\n    pub fn m(&self) {}\n}\n\n\
         #[cfg(feature = \"x\")]\npub fn g() {}\n\
         #[cfg(not(feature = \"x\"))]\npub fn g() {}\n\n\
         pub fn h() {\n    g();\n}\n",
    )
    .expect("write lib.rs");
    fs::write(
        repo.join("tests/t.rs"),
        "use old_name::Set;\n\n#[test]\nfn f() {\n    let s: Set = Set;\n    s.m();\n}\n",
    )
    .expect("write t.rs");
    (repo, base)
}

/// Without `cargo` every pass records the same empty map, so these tests
/// would not see the map move (Claude review of #478).
fn cargo_available() -> bool {
    std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn write_cargo_toml(repo: &Path, crate_name: &str, features: &str) {
    fs::write(
        repo.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n{features}[workspace]\n"
        ),
    )
    .expect("write Cargo.toml");
}

/// `outcome` plus what the Cargo map writes: the Cargo columns of every file,
/// the `cfg_active` verdict of every twin, the crate evidence, and the
/// coverage receipt apart from the name of the path that ran.
fn cargo_outcome(resp: &Value) -> Value {
    let path = PathBuf::from(resp["graph_path"].as_str().expect("graph_path"));
    let store = GraphStore::open_or_create(&path).expect("open graph");
    let q = "MATCH (f:File) RETURN f.id, f.target_context, f.target_owners, \
             f.cfg_gate, f.cfg_active, f.module_path";
    let mut files: Vec<String> = store
        .execute_query(q)
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .rows
        .iter()
        .map(|r| r.join("|"))
        .collect();
    files.sort();
    let mut twins = Vec::new();
    for label in CFG_GATE_LABELS {
        let q = format!("MATCH (n:{label}) WHERE n.id CONTAINS '#cfg(' RETURN n.id, n.cfg_active");
        if let Ok(rows) = store.execute_query(&q) {
            twins.extend(rows.rows.iter().map(|r| format!("{label}|{}", r.join("|"))));
        }
    }
    twins.sort();
    json!({
        "outcome": outcome(resp),
        "files": files,
        "twins": twins,
        "evidence": serde_json::to_value(store.crate_evidence()).expect("evidence"),
        "coverage_files": resp["coverage"]["files"],
        "cargo_attribution": resp["coverage"]["cargo_attribution"],
    })
}

/// Analyzes `repo` into `out`, applies `edit`, analyzes again, and checks the
/// second call refreshed the graph into what a fresh analysis builds, and that
/// the edit changed what the Cargo map makes of the tree.
fn assert_refreshed_like_fresh(prefix: &str, edit: impl FnOnce(&Path)) -> Value {
    let (repo, base) = cargo_fixture(prefix, "old_name");
    let out = base.join("out");
    let first = analyze(&repo, &out, json!({}));
    let before = cargo_outcome(&first);
    edit(&repo);
    let second = analyze(&repo, &out, json!({}));
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(fresh["index"]["mode"], "full", "{fresh}");
    let expected = cargo_outcome(&fresh);
    assert_ne!(before, expected, "precondition: the edit must move the map");
    assert_eq!(cargo_outcome(&second), expected);
    second
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
    assert_eq!(second["index"]["mode"], "incremental", "{second}");
    assert_eq!(second["index"]["files_reparsed"], 0, "{second}");
    assert_eq!(cargo_outcome(&second), cargo_outcome(&first));
}

#[test]
fn a_renamed_crate_is_refreshed_like_a_fresh_analysis() {
    // The Codex review of #478 case: only `Cargo.toml` changes, a file the walk
    // does not list. The unchanged `tests/t.rs` names the old crate, so its
    // receiver no longer resolves through the `use`.
    if !cargo_available() {
        return;
    }
    let second = assert_refreshed_like_fresh("analyze443_cargo_rename_", |repo| {
        write_cargo_toml(repo, "new_name", "");
    });
    assert_eq!(second["index"]["files_reparsed"], 0, "{second}");
}

#[test]
fn a_default_feature_is_refreshed_like_a_fresh_analysis() {
    // A `[features] default` edit flips which twin of `g` the build compiles,
    // so the twins' `cfg_active` and the twin `h` calls move with no file
    // reparsed (issue #353).
    if !cargo_available() {
        return;
    }
    let second = assert_refreshed_like_fresh("analyze443_cargo_feature_", |repo| {
        write_cargo_toml(
            repo,
            "old_name",
            "[features]\ndefault = [\"x\"]\nx = []\n\n",
        );
    });
    assert_eq!(second["index"]["files_reparsed"], 0, "{second}");
}

#[test]
fn an_added_rust_file_is_refreshed_like_a_fresh_analysis() {
    // Claude review of #523: an added `.rs` file is a new key of the map. A
    // module of the library and an integration test, which is a new target.
    if !cargo_available() {
        return;
    }
    let second = assert_refreshed_like_fresh("analyze443_cargo_add_", |repo| {
        fs::write(repo.join("src/extra.rs"), "pub fn e() {}\n").expect("add extra.rs");
        let lib = fs::read_to_string(repo.join("src/lib.rs")).expect("read lib.rs");
        fs::write(repo.join("src/lib.rs"), format!("pub mod extra;\n{lib}")).expect("edit lib.rs");
        fs::write(
            repo.join("tests/u.rs"),
            "use old_name::extra::e;\n\n#[test]\nfn u() {\n    e();\n}\n",
        )
        .expect("add u.rs");
    });
    assert_eq!(second["index"]["added"], 2, "{second}");
}

#[test]
fn a_renamed_rust_file_is_refreshed_like_a_fresh_analysis() {
    // A renamed integration test is a target that leaves and one that comes.
    if !cargo_available() {
        return;
    }
    let second = assert_refreshed_like_fresh("analyze443_cargo_rename_file_", |repo| {
        fs::rename(repo.join("tests/t.rs"), repo.join("tests/renamed.rs")).expect("rename");
    });
    assert_eq!(second["index"]["renamed"], 1, "{second}");
}

#[test]
fn a_deleted_rust_file_is_refreshed_like_a_fresh_analysis() {
    if !cargo_available() {
        return;
    }
    let second = assert_refreshed_like_fresh("analyze443_cargo_delete_", |repo| {
        fs::remove_file(repo.join("tests/t.rs")).expect("delete");
    });
    assert_eq!(second["index"]["deleted"], 1, "{second}");
}

#[test]
fn an_index_codebase_pass_that_sees_the_cargo_map_move_keeps_the_scope() {
    // Claude review of #478: the `index_codebase` route. Its pass refreshes
    // the facts and now keeps the scope, so the next analysis reuses the graph
    // and still matches a fresh one.
    if !cargo_available() {
        return;
    }
    let (repo, base) = cargo_fixture("analyze443_cargo_ix_", "old_name");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    write_cargo_toml(&repo, "new_name", "");
    let mut args = full_args(&repo, &out);
    args["full"] = json!(false);
    do_index_codebase(&args).expect("index");
    let m = crate::indexer::manifest::load(&crate::indexer::manifest::manifest_path(&out))
        .expect("manifest");
    assert!(m.scope.is_some(), "the pass vouches for the graph");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "incremental", "{resp}");
    let fresh = analyze(&repo, &base.join("fresh"), json!({}));
    assert_eq!(cargo_outcome(&resp), cargo_outcome(&fresh));
}

#[test]
fn a_manifest_written_with_a_cargo_digest_still_loads() {
    // The manifests written before this change carry `cargo_facts`; the field
    // is gone, and serde ignores it, so such a manifest is still reused.
    if !cargo_available() {
        return;
    }
    let (repo, base) = cargo_fixture("analyze443_cargo_old_manifest_", "old_name");
    let out = base.join("out");
    analyze(&repo, &out, json!({}));
    let path = crate::indexer::manifest::manifest_path(&out);
    let mut m: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
    assert!(m.get("cargo_facts").is_none(), "no longer written: {m}");
    m["cargo_facts"] = json!("0".repeat(64));
    fs::write(&path, m.to_string()).expect("write");
    let resp = analyze(&repo, &out, json!({}));
    assert_eq!(resp["index"]["mode"], "incremental", "{resp}");
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
