// Issue #58 — ingest_traces (matched / static-miss / unresolved), the
// FILE_CHANGES_WITH surface in get_impact, and query_graph reachability of
// both new edge families, exercised end to end through the real handlers.
use super::*;
use std::fs;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Builds a git repo where caller.py calls callee.py::callee (resolvable) and
/// the two files co-change 3× (for FILE_CHANGES_WITH). Returns (tmp, graph).
fn fixture() -> (tempfile::TempDir, String) {
    let tmp = tempfile::Builder::new()
        .prefix("temporal_rt_")
        .tempdir()
        .expect("tmp");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).expect("mk repo");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@ap.dev"]);
    git(&repo, &["config", "user.name", "AP"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    let callee_body =
        |n: i32| format!("def callee():\n    return {n}\n\ndef other():\n    return 2\n");
    fs::write(repo.join("callee.py"), callee_body(1)).unwrap();
    fs::write(
        repo.join("caller.py"),
        "from callee import callee\n\ndef caller():\n    return callee()\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c1"]);
    // Two more commits touching BOTH files → 3 co-changes total.
    for i in 2..4 {
        fs::write(repo.join("callee.py"), callee_body(i)).unwrap();
        fs::write(
            repo.join("caller.py"),
            format!("from callee import callee\n\ndef caller():\n    return callee() + {i}\n"),
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).expect("mk out");
    let idx = do_index_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "full": true,
    }))
    .expect("index");
    // The index mined co-change (git repo, default on).
    assert_eq!(
        idx["cochange"]["mode"],
        json!("full"),
        "cochange mined on full index"
    );
    let graph = idx["graph_path"].as_str().expect("graph_path").to_string();
    // Resolve so the static Calls edge caller->callee exists.
    // Surface the store-open error's full text (which now carries the
    // storage-condition annotation from write_diagnostics) and name the
    // path, so an ENOSPC is distinguishable from a genuine key-not-found
    // (issue #143).
    let store = graph_store::GraphStore::open_or_create(Path::new(&graph))
        .unwrap_or_else(|e| panic!("open graph store at {graph}: {e}"));
    resolver::resolve_graph(&store).expect("resolve");
    drop(store);
    (tmp, graph)
}

#[test]
fn ingest_traces_matches_creates_and_reports_unresolved() {
    let (_tmp, graph) = fixture();
    let resp = run_ingest_traces(&json!({
        "graph_path": graph,
        "traces": [
            { "caller": "caller.py::caller", "callee": "callee.py::callee", "count": 5 },
            { "caller": "caller.py::caller", "callee": "callee.py::other", "count": 3 },
            { "caller": "ghost.py::nope", "callee": "callee.py::callee" }
        ]
    }));
    assert_eq!(resp["status"], json!("ok"), "resp: {resp}");
    assert_eq!(
        resp["matched"],
        json!(1),
        "the static caller->callee call is annotated"
    );
    assert_eq!(
        resp["unmatched_created"],
        json!(1),
        "caller->other has no static edge → OBSERVED_CALLS"
    );
    assert_eq!(
        resp["unresolved_names"],
        json!(1),
        "ghost::nope is not a node"
    );

    // The static Calls edge now carries the observed weight.
    let weighted = run_query_graph(&json!({
        "graph_path": graph,
        "query": "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) \
                  WHERE a.id = 'caller.py::caller' AND b.id = 'callee.py::callee' \
                  RETURN r.observed_count"
    }));
    assert_eq!(
        weighted["rows"][0][0],
        json!("5"),
        "observed_count annotated: {weighted}"
    );

    // The divergence is a queryable OBSERVED_CALLS edge (query_graph surface).
    let observed = run_query_graph(&json!({
        "graph_path": graph,
        "query": "MATCH (a:Function)-[r:OBSERVED_CALLS_Function_Function]->(b:Function) \
                  RETURN a.id, b.id, r.observed_count"
    }));
    let rows = observed["rows"].as_array().expect("rows");
    assert!(
        rows.iter()
            .any(|r| r[0] == "caller.py::caller" && r[1] == "callee.py::other"),
        "OBSERVED_CALLS divergence must be reachable via query_graph: {observed}"
    );
}

#[test]
fn get_impact_surfaces_cochange_partners_under_token_surface() {
    let (_tmp, graph) = fixture();
    // callee.py::callee is called by caller; its file co-changes with caller.py.
    let full = run_get_impact(&json!({
        "graph_path": graph, "qualified_name": "callee.py::callee"
    }));
    let partners = full["cochange_partners"]
        .as_array()
        .expect("cochange_partners");
    assert!(
        partners.iter().any(|p| p["file"] == "caller.py"),
        "callee.py's co-change partner caller.py must be listed: {}",
        full["cochange_partners"]
    );

    // ids mode (issue #56 renderer) applies to the section automatically.
    let ids = run_get_impact(&json!({
        "graph_path": graph, "qualified_name": "callee.py::callee", "detail": "ids"
    }));
    let ids_partners = ids["cochange_partners"].as_array().expect("array");
    assert!(
        ids_partners.iter().all(|v| v.is_string()),
        "ids mode must render co-change partners as bare file ids: {}",
        ids["cochange_partners"]
    );
    assert!(ids_partners.iter().any(|v| v == "caller.py"));
}

#[test]
fn file_changes_with_is_reachable_via_query_graph() {
    let (_tmp, graph) = fixture();
    let resp = run_query_graph(&json!({
        "graph_path": graph,
        "query": "MATCH (a:File)-[r:FILE_CHANGES_WITH]-(b:File) \
                  RETURN a.id, b.id, r.cochange_count, r.coupling"
    }));
    let rows = resp["rows"].as_array().expect("rows");
    assert!(
        !rows.is_empty(),
        "the mined FILE_CHANGES_WITH edge must be queryable: {resp}"
    );
    // caller.py <-> callee.py co-changed in all 3 commits.
    assert!(
        rows.iter().any(|r| {
            let files = [r[0].as_str().unwrap_or(""), r[1].as_str().unwrap_or("")];
            files.contains(&"caller.py") && files.contains(&"callee.py")
        }),
        "caller.py <-> callee.py must be a co-change edge: {resp}"
    );
}
