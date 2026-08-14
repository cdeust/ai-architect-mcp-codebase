// temporal_runtime_edges — issue #58 gate (library level).
//
// External proof for FILE_CHANGES_WITH co-change mining: build a repo with a
// REAL git history whose co-change pairs are known by construction, mine it, and
// assert the EXACT edges + properties. Then extend the history and mine
// incrementally, asserting the aggregates grew rather than being recomputed from
// a re-walk (the append-only contract). Also asserts the edges are reachable via
// a plain Cypher MATCH (the query_graph surface).

use ai_architect_mcp::cochange::{self, Mode};
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, IndexOptions};
use std::path::Path;
use std::process::Command;

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

fn write(repo: &Path, name: &str, body: &str) {
    std::fs::write(repo.join(name), body).expect("write file");
}

fn commit(repo: &Path, msg: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", msg]);
}

/// cochange_count on the FILE_CHANGES_WITH edge between two files (stored a<b),
/// or None if no edge.
fn cochange_count(store: &GraphStore, a: &str, b: &str) -> Option<i64> {
    let q = format!(
        "MATCH (x:File)-[r:FILE_CHANGES_WITH]-(y:File) \
         WHERE x.id = '{a}' AND y.id = '{b}' RETURN r.cochange_count"
    );
    let qr = store.execute_query(&q).ok()?;
    qr.rows
        .first()
        .and_then(|row| row.first())
        .and_then(|c| c.parse::<i64>().ok())
}

fn index(repo: &Path, out: &Path) -> std::path::PathBuf {
    let graph = out.join("graph");
    indexer::index_codebase_with_language(repo, &graph, &IndexOptions::default()).expect("index");
    graph
}

#[test]
fn co_change_mining_emits_exact_expected_edges() {
    let tmp = tempfile::Builder::new()
        .prefix("cochange_exact_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mk repo");
    init_git(&repo);

    // Construct a known co-change history:
    //   c1..c4: a.py AND b.py change together  → (a,b) co-change = 4
    //   c5:     a.py alone                     → changes_a = 5, changes_b = 4
    //   c6:     c.py AND d.py together (once)   → (c,d) co-change = 1 (below the min)
    write(&repo, "a.py", "def a0():\n    return 0\n");
    write(&repo, "b.py", "def b0():\n    return 0\n");
    commit(&repo, "c1 a+b");
    for i in 1..4 {
        write(&repo, "a.py", &format!("def a0():\n    return {i}\n"));
        write(&repo, "b.py", &format!("def b0():\n    return {i}\n"));
        commit(&repo, &format!("c{} a+b", i + 1));
    }
    write(&repo, "a.py", "def a0():\n    return 99\n");
    commit(&repo, "c5 a only");
    write(&repo, "c.py", "def c0():\n    return 0\n");
    write(&repo, "d.py", "def d0():\n    return 0\n");
    commit(&repo, "c6 c+d");

    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).expect("mk out");
    let graph = index(&repo, &out);
    let store = GraphStore::open_or_create(&graph).expect("open");

    let result = cochange::mine(&store, &repo, &out, Mode::Full, None)
        .expect("mine")
        .expect("git repo");
    assert_eq!(result.mode, "full");

    // (a,b) cleared the thresholds: 4 co-changes, coupling 4/min(5,4)=1.0.
    assert_eq!(
        cochange_count(&store, "a.py", "b.py"),
        Some(4),
        "a.py <-> b.py must have exactly 4 co-changes"
    );
    // Verify the derived properties.
    let props = store
        .execute_query(
            "MATCH (x:File)-[r:FILE_CHANGES_WITH]-(y:File) \
             WHERE x.id = 'a.py' AND y.id = 'b.py' \
             RETURN r.support, r.coupling, r.jaccard",
        )
        .expect("props query");
    let row = &props.rows[0];
    assert_eq!(row[0], "4", "support = min(changes_a=5, changes_b=4) = 4");
    assert!(
        (row[1].parse::<f64>().unwrap() - 1.0).abs() < 1e-6,
        "coupling = 4/4 = 1.0, got {}",
        row[1]
    );
    assert!(
        (row[2].parse::<f64>().unwrap() - 0.8).abs() < 1e-6,
        "jaccard = 4/(5+4-4) = 0.8, got {}",
        row[2]
    );

    // (c,d) had only 1 co-change → below the min-3 threshold → NO edge.
    assert_eq!(
        cochange_count(&store, "c.py", "d.py"),
        None,
        "c.py <-> d.py (1 co-change) must NOT be an edge"
    );

    // Exactly one FILE_CHANGES_WITH edge total (a<->b).
    let total = store
        .execute_query("MATCH ()-[r:FILE_CHANGES_WITH]->() RETURN count(r)")
        .expect("count");
    assert_eq!(total.rows[0][0], "1", "exactly one co-change edge expected");
    assert_eq!(result.edges_written, 1);
}

#[test]
fn incremental_mining_extends_the_aggregates() {
    let tmp = tempfile::Builder::new()
        .prefix("cochange_incr_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mk repo");
    init_git(&repo);
    write(&repo, "a.py", "def a0():\n    return 0\n");
    write(&repo, "b.py", "def b0():\n    return 0\n");
    commit(&repo, "c1");
    for i in 1..3 {
        write(&repo, "a.py", &format!("def a0():\n    return {i}\n"));
        write(&repo, "b.py", &format!("def b0():\n    return {i}\n"));
        commit(&repo, &format!("c{}", i + 1));
    }

    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).expect("mk out");
    let graph = index(&repo, &out);
    let store = GraphStore::open_or_create(&graph).expect("open");

    // Full mine of the first 3 commits → (a,b) = 3.
    cochange::mine(&store, &repo, &out, Mode::Full, None)
        .expect("mine full")
        .expect("git");
    assert_eq!(cochange_count(&store, "a.py", "b.py"), Some(3));

    // Two MORE co-change commits.
    for i in 3..5 {
        write(&repo, "a.py", &format!("def a0():\n    return {i}\n"));
        write(&repo, "b.py", &format!("def b0():\n    return {i}\n"));
        commit(&repo, &format!("c{}", i + 1));
    }

    // Incremental mine: load the prior state, extend with only the new commits.
    let prior = cochange::load_state(&cochange::state_path(&out)).expect("prior state");
    let result = cochange::mine(&store, &repo, &out, Mode::Incremental, Some(prior))
        .expect("mine incremental")
        .expect("git");
    assert_eq!(result.mode, "incremental");
    assert_eq!(
        result.commits_scanned, 2,
        "incremental must scan ONLY the 2 new commits, not re-walk all 5"
    );
    assert_eq!(
        cochange_count(&store, "a.py", "b.py"),
        Some(5),
        "aggregates must EXTEND (3 + 2) not recompute from a partial walk"
    );
}
