// artifact_incremental_fill — issue #55 completion gate (library level).
//
// External done-criteria: a teammate who clones a repo carrying a *stale*
// committed artifact must end up with the SAME graph as a from-scratch full
// index of HEAD — by bootstrap-importing the snapshot and then incrementally
// filling only the artifact→HEAD diff, in a fraction of a full index's time.
//
// This test asserts CORRECTNESS only (a deterministic gate that must be green on
// every machine and profile — §13 G3, §6.2). The PERFORMANCE claim ("fill is
// much cheaper than full") lives in `benchmarks/incremental_speed/`, with
// recorded hardware/toolchain and committed results — NOT in this gate. A
// wall-clock ratio here was machine- and profile-dependent (issue #74: 31–79% vs
// a ≤30% gate under --release, where a fast `full` denominator lets the fill's
// fixed costs dominate the ratio), so a timing miss reported as a functional
// failure and took the suite red.
//
// This test:
//   1. builds a git fixture repo,
//   2. full-indexes it and exports the artifact (graph.zst + bundled manifest),
//   3. commits the artifact, then commits further changes (edit/add/delete/
//      rename) so the artifact is several commits stale,
//   4. `git clone`s the repo → a fresh clone carrying the committed artifact,
//   5. bootstrap-imports the artifact into a fresh output dir, then runs the
//      incremental fill of the artifact→working-tree diff,
//   6. asserts query parity with a from-scratch full index of the clone's HEAD,
//   7. asserts the fill reported the exact change partition and used git diff.

use ai_architect_mcp::artifact;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, manifest, FillMethod, FillResult, IndexOptions};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// Enough files for a meaningful parity check across edit/add/delete/rename; the
// large fixture that used to give a timing margin is no longer needed here (the
// perf claim moved to benchmarks/incremental_speed/).
const N_MODULES: usize = 20;

fn module_body(i: usize) -> String {
    let mut s = String::new();
    for j in 0..8 {
        s.push_str(&format!(
            "def mod{i}_fn{j}(x):\n    return x + {i} + {j}\n\n"
        ));
    }
    for k in 0..3 {
        s.push_str(&format!(
            "class Widget{i}_{k}:\n    def spin(self):\n        return {i}\n    \
             def flip(self, y):\n        return y\n\n"
        ));
    }
    s
}

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

fn init_git(repo: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@ap.dev"]);
    git(repo, &["config", "user.name", "AP Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

fn snapshot_queries(store: &GraphStore) -> Vec<(String, Vec<Vec<String>>)> {
    let queries = [
        "MATCH (n) RETURN count(n)",
        "MATCH ()-[r]->() RETURN count(r)",
        "MATCH (f:Function) RETURN f.name ORDER BY f.name",
        "MATCH (m:Method) RETURN m.name ORDER BY m.name",
        "MATCH (c:Struct) RETURN c.name ORDER BY c.name",
        "MATCH (f:File) RETURN f.id ORDER BY f.id",
        "MATCH (d:Directory) RETURN d.id ORDER BY d.id",
        "MATCH (df:File)-[:Defines_File_Function]->(fn:Function) \
         RETURN df.id, fn.name ORDER BY df.id, fn.name",
    ];
    queries
        .iter()
        .map(|q| {
            let qr = store
                .execute_query(q)
                .unwrap_or_else(|e| panic!("query failed [{q}]: {e}"));
            (q.to_string(), qr.rows)
        })
        .collect()
}

/// -- 1. Fixture repo, initial commit -------------------------------------
fn write_and_commit_initial_fixture(repo: &Path) {
    fs::create_dir_all(repo.join("src")).expect("mk repo/src");
    for i in 0..N_MODULES {
        fs::write(repo.join(format!("src/mod{i:03}.py")), module_body(i)).expect("write module");
    }
    init_git(repo);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "initial"]);
}

/// -- 2. Full index + export artifact (with bundled manifest), then commit
/// it -----------------------------------------------------------------------
fn export_and_commit_artifact(tmp: &Path, repo: &Path) {
    let idx_out = tmp.join("idx_out");
    fs::create_dir_all(&idx_out).expect("mk idx_out");
    let idx_graph = idx_out.join("graph");
    let idx_manifest = manifest::manifest_path(&idx_out);
    let result = indexer::index_codebase_with_language(repo, &idx_graph, &IndexOptions::default())
        .expect("full index");
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

/// -- 3. edit / add / delete / rename, all committed → the artifact is now
/// stale. -----------------------------------------------------------------
fn mutate_and_commit_source_changes(repo: &Path) {
    fs::write(
        repo.join("src/mod000.py"),
        "def brand_new_fn():\n    return 999\n\ndef another_new():\n    return 0\n",
    )
    .expect("edit");
    fs::write(
        repo.join("src/mod_added.py"),
        "def added_symbol():\n    return 1\n",
    )
    .expect("add");
    fs::remove_file(repo.join("src/mod001.py")).expect("delete");
    fs::rename(
        repo.join("src/mod002.py"),
        repo.join("src/mod002_renamed.py"),
    )
    .expect("rename");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "changes"]);
}

/// -- 4. Simulate a fresh clone (carries the committed artifact). Returns
/// the clone's path.
fn clone_repo(tmp: &Path, repo: &Path) -> PathBuf {
    let clone = tmp.join("clone");
    let out = Command::new("git")
        .args(["clone", "-q"])
        .arg(repo)
        .arg(&clone)
        .output()
        .expect("git clone");
    assert!(
        out.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        artifact::artifact_exists(&clone),
        "the cloned repo must carry the committed artifact"
    );
    clone
}

/// -- 5. Bootstrap-import then incremental fill. Returns (boot_graph, fill).
fn bootstrap_and_fill(tmp: &Path, clone: &Path) -> (PathBuf, FillResult) {
    let boot_out = tmp.join("boot_out");
    fs::create_dir_all(&boot_out).expect("mk boot_out");
    let boot_graph = boot_out.join("graph");
    let boot_manifest = manifest::manifest_path(&boot_out);
    assert!(
        !boot_graph.exists(),
        "fresh clone starts with no local graph"
    );

    let meta = artifact::import_artifact(clone, &boot_graph).expect("bootstrap import");
    assert!(boot_graph.exists(), "import materialises the graph");
    assert!(
        boot_manifest.exists(),
        "import restores the bundled manifest beside the graph"
    );
    let imported_manifest = manifest::load(&boot_manifest);

    let fill = indexer::fill_after_bootstrap(
        clone,
        &boot_graph,
        &boot_manifest,
        &meta.commit,
        imported_manifest.as_ref(),
        &IndexOptions::default(),
    )
    .expect("incremental fill");
    (boot_graph, fill)
}

/// -- 7. Fill reported the exact change partition, via git diff ----------
fn assert_fill_partition_via_git_diff(fill: &FillResult) {
    assert_eq!(
        fill.method,
        FillMethod::GitDiff,
        "clone is a git tree → git diff"
    );
    assert_eq!(fill.result.changed, 1, "mod000.py edited");
    assert_eq!(fill.result.added, 1, "mod_added.py");
    assert_eq!(fill.result.deleted, 1, "mod001.py removed");
    assert_eq!(fill.result.renamed, 1, "mod002.py -> mod002_renamed.py");
    assert_eq!(fill.result.files_reparsed, 3, "changed+added+renamed-new");
}

/// -- 6. Parity: filled graph == a from-scratch full index of HEAD -------
fn assert_fill_parity_with_full_index(tmp: &Path, clone: &Path, boot_graph: &Path) {
    let full_out = tmp.join("full_out");
    fs::create_dir_all(&full_out).expect("mk full_out");
    let full_graph = full_out.join("graph");
    indexer::index_codebase_with_language(clone, &full_graph, &IndexOptions::default())
        .expect("full index of clone HEAD");
    let snap_boot = {
        let store = GraphStore::open_or_create(boot_graph).expect("open filled graph");
        snapshot_queries(&store)
    };
    let snap_full = {
        let store = GraphStore::open_or_create(&full_graph).expect("open full graph");
        snapshot_queries(&store)
    };
    assert_eq!(
        snap_boot, snap_full,
        "bootstrap+fill graph must equal a from-scratch full index of HEAD, query-for-query"
    );
}

#[test]
fn stale_clone_bootstrap_fill_matches_full_index() {
    let tmp = tempfile::Builder::new()
        .prefix("artifact_fill_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");

    write_and_commit_initial_fixture(&repo);
    export_and_commit_artifact(tmp.path(), &repo);
    mutate_and_commit_source_changes(&repo);

    let clone = clone_repo(tmp.path(), &repo);
    let (boot_graph, fill) = bootstrap_and_fill(tmp.path(), &clone);

    assert_fill_partition_via_git_diff(&fill);
    assert_fill_parity_with_full_index(tmp.path(), &clone, &boot_graph);
}

fn write_and_export_nogit_fixture(tmp: &Path, repo: &Path) {
    fs::create_dir_all(repo.join("src")).expect("mk src");
    for i in 0..12 {
        fs::write(repo.join(format!("src/m{i}.py")), module_body(i)).expect("write");
    }

    let idx_out = tmp.join("idx_out");
    fs::create_dir_all(&idx_out).expect("mk idx_out");
    let idx_graph = idx_out.join("graph");
    let idx_manifest = manifest::manifest_path(&idx_out);
    let result = indexer::index_codebase_with_language(repo, &idx_graph, &IndexOptions::default())
        .expect("index");
    indexer::write_full_manifest(repo, &idx_manifest, &IndexOptions::default()).expect("manifest");
    artifact::export_artifact(
        &idx_graph,
        repo,
        result.node_count,
        result.edge_count,
        Some(&idx_manifest),
        None,
    )
    .expect("export");
}

/// Bootstrap-imports + fills `repo` (no git → content-hash fallback).
/// Returns (boot_graph, fill).
fn bootstrap_and_fill_nogit(tmp: &Path, repo: &Path) -> (PathBuf, FillResult) {
    let boot_out = tmp.join("boot_out");
    fs::create_dir_all(&boot_out).expect("mk boot_out");
    let boot_graph = boot_out.join("graph");
    let boot_manifest = manifest::manifest_path(&boot_out);
    let meta = artifact::import_artifact(repo, &boot_graph).expect("import");
    let imported = manifest::load(&boot_manifest);
    let fill = indexer::fill_after_bootstrap(
        repo,
        &boot_graph,
        &boot_manifest,
        &meta.commit,
        imported.as_ref(),
        &IndexOptions::default(),
    )
    .expect("fill");
    (boot_graph, fill)
}

#[test]
fn fill_falls_back_to_content_hash_when_not_a_git_tree() {
    // When git can't diff (here: the codebase is not a git working tree), the
    // fill classifies against the bundled manifest's content hashes and still
    // reaches parity with a full index of the current tree.
    let tmp = tempfile::Builder::new()
        .prefix("artifact_fill_nogit_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo"); // NOT a git repo
    write_and_export_nogit_fixture(tmp.path(), &repo);

    // Mutate after export (no git): edit + add + delete.
    fs::write(repo.join("src/m0.py"), "def changed0():\n    return 1\n").expect("edit");
    fs::write(repo.join("src/m_new.py"), "def newsym():\n    return 2\n").expect("add");
    fs::remove_file(repo.join("src/m1.py")).expect("delete");

    let (boot_graph, fill) = bootstrap_and_fill_nogit(tmp.path(), &repo);
    assert_eq!(
        fill.method,
        FillMethod::ContentHash,
        "no git tree → content-hash classification"
    );
    assert_eq!(fill.result.changed, 1);
    assert_eq!(fill.result.added, 1);
    assert_eq!(fill.result.deleted, 1);

    // Parity with a full index of the current tree.
    let full_out = tmp.path().join("full_out");
    fs::create_dir_all(&full_out).expect("mk full_out");
    let full_graph = full_out.join("graph");
    indexer::index_codebase_with_language(&repo, &full_graph, &IndexOptions::default())
        .expect("full");
    let snap_boot = {
        let s = GraphStore::open_or_create(&boot_graph).expect("open boot");
        snapshot_queries(&s)
    };
    let snap_full = {
        let s = GraphStore::open_or_create(&full_graph).expect("open full");
        snapshot_queries(&s)
    };
    assert_eq!(
        snap_boot, snap_full,
        "content-hash fill must also reach full-index parity"
    );
}
