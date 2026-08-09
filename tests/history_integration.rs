// history_integration — end-to-end test for the temporal layer.
//
// Builds a real git repo with two commits, indexes it, runs index_history,
// and verifies the version spine is traversable in BOTH directions:
//   File  <-VersionOf-  Version  -ChangedIn->  Commit  -PreviousVersion->  Commit
// plus the reverse (a commit's changed files; an entity's prior version).
//
// source: second-brain history requirement — full symbol-level versioning.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::history;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("git must be available");
    assert!(status.success(), "git {args:?} failed");
}

fn count(store: &GraphStore, cypher: &str) -> u64 {
    let qr = store.execute_query(cypher).unwrap();
    qr.rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

const LIB_V1: &str = r#"
pub fn alpha() -> i32 {
    1
}

pub fn beta() -> i32 {
    2
}
"#;

// beta's body changes; alpha is untouched.
const LIB_V2: &str = r#"
pub fn alpha() -> i32 {
    1
}

pub fn beta() -> i32 {
    2 + 40
}
"#;

const EXTRA: &str = r#"
pub fn gamma() -> i32 {
    3
}
"#;

#[test]
fn test_index_history_builds_traversable_version_spine() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let repo = tempfile::Builder::new()
        .prefix("history_it_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = std::fs::remove_dir_all(&repo);
    let src = repo.join("src");
    std::fs::create_dir_all(&src).unwrap();

    // --- Build a two-commit repo -------------------------------------------
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@example.com"]);

    std::fs::write(src.join("lib.rs"), LIB_V1).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial: alpha + beta"]);

    std::fs::write(src.join("lib.rs"), LIB_V2).unwrap();
    std::fs::write(src.join("extra.rs"), EXTRA).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "change beta, add gamma"]);

    // --- Index the HEAD snapshot from the repo root so File ids == git paths.
    let graph = repo.join("graph");
    indexer::index_codebase(&repo, &graph).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph).unwrap();
    resolver::resolve_graph(&store).expect("resolve_graph");

    // --- Ingest history ----------------------------------------------------
    let result = history::index_history(&store, &repo, None).expect("index_history");
    assert_eq!(
        result.commits, 2,
        "two commits expected, got {}",
        result.commits
    );
    assert!(result.versions > 0, "version nodes must be created");

    // Commit lineage: commit2 -PreviousVersion-> commit1.
    assert_eq!(
        count(
            &store,
            "MATCH ()-[r:PreviousVersion_Commit_Commit]->() RETURN count(r)"
        ),
        1,
        "exactly one commit-ancestry edge expected"
    );

    // Every Version points to a Commit (ChangedIn) and to an entity (VersionOf).
    assert!(
        count(
            &store,
            "MATCH ()-[r:ChangedIn_Version_Commit]->() RETURN count(r)"
        ) > 0,
        "ChangedIn edges must exist"
    );
    assert!(
        count(
            &store,
            "MATCH ()-[r:VersionOf_Version_File]->() RETURN count(r)"
        ) > 0,
        "VersionOf(File) edges must exist"
    );

    // src/lib.rs changed in BOTH commits, so its versions chain.
    assert!(
        count(
            &store,
            "MATCH ()-[r:PreviousVersion_Version_Version]->() RETURN count(r)"
        ) > 0,
        "an entity changed in two commits must have a PreviousVersion chain"
    );

    // --- Forward traversal: File -> its versions -> the commits that made them.
    let fwd = store.execute_query(
        "MATCH (f:File)<-[:VersionOf_Version_File]-(v:Version)-[:ChangedIn_Version_Commit]->(c:Commit) \
         WHERE f.path = 'src/lib.rs' \
         RETURN c.message"
    ).unwrap();
    assert!(
        fwd.rows.len() >= 2,
        "src/lib.rs should have >=2 versions across commits, got {}",
        fwd.rows.len()
    );
    let messages: Vec<&str> = fwd
        .rows
        .iter()
        .filter_map(|r| r.first().map(|s| s.as_str()))
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("initial")),
        "lib.rs history must include the initial commit; got {messages:?}"
    );

    // --- Reverse traversal: Commit -> the entities it changed --------------
    let rev = store
        .execute_query(
            "MATCH (c:Commit)<-[:ChangedIn_Version_Commit]-(v:Version) \
         WHERE c.message = 'change beta, add gamma' \
         RETURN v.qualified_name, v.entity_kind",
        )
        .unwrap();
    let changed: Vec<String> = rev.rows.iter().filter_map(|r| r.first().cloned()).collect();
    assert!(
        changed.iter().any(|q| q == "src/extra.rs"),
        "the second commit added src/extra.rs; reverse traversal must surface it, got {changed:?}"
    );
    assert!(
        changed.iter().any(|q| q == "src/lib.rs"),
        "the second commit modified src/lib.rs; got {changed:?}"
    );

    // --- Symbol-level: beta changed in commit 2, alpha did not -------------
    // beta's body changed, so a Version of the beta function must attach to
    // the second commit; alpha (untouched) must not.
    let sym = store
        .execute_query(
            "MATCH (c:Commit)<-[:ChangedIn_Version_Commit]-(v:Version) \
         WHERE c.message = 'change beta, add gamma' AND v.entity_kind = 'Function' \
         RETURN v.qualified_name",
        )
        .unwrap();
    let changed_fns: Vec<String> = sym.rows.iter().filter_map(|r| r.first().cloned()).collect();
    assert!(
        changed_fns.iter().any(|q| q.ends_with("::beta")),
        "beta changed in commit 2 and must have a Version; got {changed_fns:?}"
    );
    assert!(
        !changed_fns.iter().any(|q| q.ends_with("::alpha")),
        "alpha was untouched in commit 2 and must NOT have a Version there; got {changed_fns:?}"
    );

    let _ = std::fs::remove_dir_all(&repo);
}
