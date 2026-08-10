// artifact_bootstrap — issue #55 end-to-end gate (library level).
//
// External done-criteria (zetetic gate): the artifact is only "done" if a
// teammate who never cold-indexed gets the SAME graph. This test:
//   1. indexes a small fixture into an original graph,
//   2. exports the compressed artifact into the (fixture = repo) tree,
//   3. simulates a fresh clone — a brand-new output dir with NO local graph,
//   4. bootstrap-imports the artifact into that fresh output dir,
//   5. asserts the graph query results match the original, query-for-query.
//
// It also asserts the git-sha staleness computation (`artifact_staleness`) that
// the handler's refuse/accept contract is built on: fresh right after export,
// and one commit behind after a subsequent commit. The refuse-and-reindex and
// accept_stale RESPONSE behaviors (cases b/c) are exercised at the handler seam
// in `src/main.rs` (module `artifact_bootstrap_tests`), which owns that logic.

use ai_architect_mcp::artifact;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use std::fs;
use std::path::Path;
use std::process::Command;

const FIXTURE_MAIN: &str = r#"
fn main() {
    helper();
}

fn helper() -> i32 {
    42
}

pub async fn serve() {
    loop {}
}
"#;

const FIXTURE_MODELS: &str = r#"
pub struct Config {
    pub name: String,
    pub retries: u32,
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Processor {
    fn process(&self, input: &str) -> String;
}
"#;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_git_repo(repo: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "test@ap.dev"]);
    git(repo, &["config", "user.name", "AP Test"]);
}

fn commit_all(repo: &Path, msg: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--no-gpg-sign", "-m", msg]);
}

/// Runs a fixed battery of read queries and returns their results as strings,
/// so two graphs can be compared query-for-query.
fn snapshot_queries(store: &GraphStore) -> Vec<(String, Vec<Vec<String>>)> {
    let queries = [
        "MATCH (f:Function) RETURN count(f)",
        "MATCH (s:Struct) RETURN s.name ORDER BY s.name",
        "MATCH (e:Enum) RETURN e.name ORDER BY e.name",
        "MATCH (t:Trait) RETURN t.name ORDER BY t.name",
        "MATCH (n) RETURN count(n)",
        "MATCH ()-[r]->() RETURN count(r)",
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

#[test]
fn fresh_clone_bootstrap_matches_original_graph() {
    let tmp = tempfile::Builder::new()
        .prefix("artifact_bootstrap_")
        .tempdir()
        .expect("create temp dir");

    // -- 1. Fixture git repo + original index -------------------------------
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("src")).expect("mk repo/src");
    fs::write(repo.join("src/main.rs"), FIXTURE_MAIN).expect("write main.rs");
    fs::write(repo.join("src/models.rs"), FIXTURE_MODELS).expect("write models.rs");
    init_git_repo(&repo);
    commit_all(&repo, "initial");

    let original_out = tmp.path().join("original_out");
    let original_graph = original_out.join("graph");
    fs::create_dir_all(&original_out).expect("mk original_out");
    let result = indexer::index_codebase(&repo.join("src"), &original_graph)
        .expect("original index should succeed");
    assert!(result.node_count > 0, "fixture must produce nodes");
    assert!(result.edge_count > 0, "fixture must produce edges");

    let original_snapshot = {
        let store = GraphStore::open_or_create(&original_graph).expect("open original graph");
        snapshot_queries(&store)
    };

    // -- 2. Export the artifact into the repo tree --------------------------
    let stats = artifact::export_artifact(
        &original_graph,
        &repo,
        result.node_count,
        result.edge_count,
        None,
        None,
    )
    .expect("export should succeed");
    assert!(stats.compressed_bytes > 0, "artifact must be non-empty");
    assert!(
        artifact::artifact_exists(&repo),
        "artifact must be detectable after export"
    );
    let gitattrs =
        fs::read_to_string(repo.join(".gitattributes")).expect("gitattributes must exist");
    assert!(
        gitattrs.contains(".ai-architect-mcp-codebase/graph.zst binary merge=ours"),
        "gitattributes must carry the merge=ours entry, got:\n{gitattrs}"
    );

    // -- Staleness computation: fresh right after export --------------------
    let meta = artifact::read_artifact_meta(&repo).expect("read sidecar");
    assert!(!meta.commit.is_empty(), "sidecar must record the git sha");
    assert!(
        artifact::artifact_staleness(&repo, &meta.commit).is_none(),
        "artifact must be fresh immediately after export"
    );

    // -- 3. Simulate a fresh clone: new output dir, NO local graph ----------
    let fresh_out = tmp.path().join("fresh_out");
    let fresh_graph = fresh_out.join("graph");
    fs::create_dir_all(&fresh_out).expect("mk fresh_out");
    assert!(
        !fresh_graph.exists(),
        "fresh clone must start without a local index"
    );

    // -- 4. Bootstrap-import ------------------------------------------------
    let imported_meta = artifact::import_artifact(&repo, &fresh_graph).expect("bootstrap import");
    assert_eq!(
        imported_meta.node_count, result.node_count,
        "sidecar node count"
    );
    assert_eq!(
        imported_meta.edge_count, result.edge_count,
        "sidecar edge count"
    );
    assert!(fresh_graph.exists(), "import must materialise the graph");

    // -- 5. Query results must match the original, query-for-query ----------
    let fresh_snapshot = {
        let store = GraphStore::open_or_create(&fresh_graph).expect("open bootstrapped graph");
        snapshot_queries(&store)
    };
    assert_eq!(
        fresh_snapshot, original_snapshot,
        "bootstrapped graph must return identical results to the cold-indexed original"
    );

    // -- Staleness computation: one commit behind after a new commit --------
    fs::write(repo.join("src/extra.rs"), "pub fn added() {}\n").expect("write extra.rs");
    commit_all(&repo, "second");
    let stale = artifact::artifact_staleness(&repo, &meta.commit)
        .expect("artifact must be stale after a new commit");
    assert_eq!(stale.artifact_sha, meta.commit);
    assert_eq!(
        stale.commits_behind,
        Some(1),
        "must report exactly one commit behind HEAD"
    );
}
