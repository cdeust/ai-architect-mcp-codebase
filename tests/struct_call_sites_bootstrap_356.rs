//! Issue #356, migration through the artifact path: a teammate clones a repo
//! whose committed artifact was written by a build that had no
//! `Calls_CallSite_Struct` table, bootstraps from it and fills the diff. The
//! fill purges and relinks the files that changed, and every step of it reads the
//! per-site tables: without the table created first, the fill fails.
//!
//! Library level, like `artifact_incremental_fill`: a git fixture, a full index,
//! the artifact exported and committed, a further commit, a clone, the import
//! and the fill.

use ai_architect_mcp::artifact;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, manifest, IndexOptions};
use std::fs;
use std::path::Path;
use std::process::Command;

const LIB: &str = "mod extra;

pub struct Tier(pub u8);

pub fn build() -> Tier {
    Tier(1)
}

pub fn build_twice() -> (Tier, Tier) {
    (Tier(1), Tier(2))
}
";

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

fn struct_rows(graph: &Path) -> i64 {
    let store = GraphStore::open_or_create(graph).expect("open graph");
    store
        .execute_query("MATCH (c:CallSite)-[r:Calls_CallSite_Struct]->(s:Struct) RETURN count(r)")
        .expect("count rows")
        .rows[0][0]
        .parse()
        .expect("a count")
}

/// The fixture repo, committed: a manifest, `lib.rs` with three constructor
/// sites and `extra.rs`, which a later commit changes.
fn committed_fixture(repo: &Path) {
    fs::create_dir_all(repo.join("src")).expect("mkdir");
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"boot356\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    fs::write(repo.join("src/lib.rs"), LIB).expect("lib");
    fs::write(repo.join("src/extra.rs"), "pub fn touched() {}\n").expect("extra");
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@ap.dev"]);
    git(repo, &["config", "user.name", "AP Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "initial"]);
}

/// A graph as an earlier build wrote it (indexed, resolved, no Struct table),
/// exported as the artifact and committed.
fn commit_artifact_without_the_struct_table(tmp: &Path, repo: &Path) {
    let idx_out = tmp.join("idx_out");
    fs::create_dir_all(&idx_out).expect("mkdir");
    let idx_graph = idx_out.join("graph");
    let idx_manifest = manifest::manifest_path(&idx_out);
    let result = indexer::index_codebase_with_language(repo, &idx_graph, &IndexOptions::default())
        .expect("full index");
    {
        let store = GraphStore::open_or_create(&idx_graph).expect("open");
        ai_architect_mcp::resolver::resolve_graph(&store).expect("resolve");
    }
    assert_eq!(
        struct_rows(&idx_graph),
        3,
        "control: three constructor sites"
    );
    {
        let store = GraphStore::open_or_create(&idx_graph).expect("open");
        store
            .execute_query("DROP TABLE Calls_CallSite_Struct")
            .expect("drop the table");
    }
    indexer::write_full_manifest(repo, &idx_manifest, &IndexOptions::default())
        .expect("write manifest");
    artifact::export_artifact(
        &idx_graph,
        repo,
        result.node_count,
        result.edge_count,
        Some(&idx_manifest),
        None,
    )
    .expect("export artifact");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "commit artifact"]);
}

/// A later commit changes a file, so the fill purges and relinks; then a clone.
fn change_and_clone(tmp: &Path, repo: &Path) -> std::path::PathBuf {
    fs::write(
        repo.join("src/extra.rs"),
        "pub fn touched() {}\npub fn another() {}\n",
    )
    .expect("edit");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "change"]);
    let clone = tmp.join("clone");
    let out = Command::new("git")
        .args(["clone", "-q"])
        .arg(repo)
        .arg(&clone)
        .output()
        .expect("git clone");
    assert!(out.status.success(), "git clone failed");
    clone
}

#[test]
fn a_bootstrap_fill_over_an_artifact_without_the_struct_table_writes_the_rows() {
    let tmp = tempfile::Builder::new()
        .prefix("struct_table_fill_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    committed_fixture(&repo);
    commit_artifact_without_the_struct_table(tmp.path(), &repo);
    let clone = change_and_clone(tmp.path(), &repo);

    let boot_out = tmp.path().join("boot_out");
    fs::create_dir_all(&boot_out).expect("mkdir");
    let boot_graph = boot_out.join("graph");
    let boot_manifest = manifest::manifest_path(&boot_out);
    let meta = artifact::import_artifact(&clone, &boot_graph).expect("bootstrap import");
    assert_eq!(
        struct_rows_or_absent(&boot_graph),
        None,
        "control: the imported graph has no Struct table"
    );
    let imported = manifest::load(&boot_manifest);
    let fill = indexer::fill_after_bootstrap(
        &clone,
        &boot_graph,
        &boot_manifest,
        &meta.commit,
        imported.as_ref(),
        &IndexOptions::default(),
    )
    .expect("the fill over a graph without the table");
    assert!(fill.result.changed >= 1, "the fill had a file to purge");
    // The fill relinks; it does not resolve. The table exists and is empty, and
    // the next resolve pass fills it.
    assert_eq!(
        struct_rows_or_absent(&boot_graph),
        Some(0),
        "the fill creates the table, empty"
    );
    {
        let store = GraphStore::open_or_create(&boot_graph).expect("open");
        ai_architect_mcp::resolver::resolve_graph(&store).expect("resolve");
    }
    assert_eq!(struct_rows(&boot_graph), 3, "the rows are backfilled");
}

/// The row count, or `None` when the table does not exist.
fn struct_rows_or_absent(graph: &Path) -> Option<i64> {
    let store = GraphStore::open_or_create(graph).expect("open graph");
    store
        .execute_query("MATCH (c:CallSite)-[r:Calls_CallSite_Struct]->(s:Struct) RETURN count(r)")
        .ok()
        .map(|q| q.rows[0][0].parse().expect("a count"))
}
