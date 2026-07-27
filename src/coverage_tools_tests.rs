// Issue #57 — the coverage-honesty tool surface: index_codebase reports
// coverage inline, index_status reports it authoritatively, and
// query_graph(graph="missed") enumerates the misses. All exercised end to end
// through the real handlers with a corrupt fixture file.
use super::*;
use std::fs;

const BROKEN: &str = "def broken(  \n    let x: i32 = ;\n    @#$%^& }{\nclass 9Invalid\n";

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("coverage_tools_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("src")).expect("mk src");
    fs::write(repo.join("src/ok.py"), "def ok():\n    return 1\n").expect("ok");
    fs::write(repo.join("src/bad.py"), BROKEN).expect("bad");
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).expect("mk out");
    (tmp, repo, out)
}

#[test]
fn index_codebase_reports_coverage_then_index_status_and_missed_surface_it() {
    let (_tmp, repo, out) = fixture();

    // index_codebase inline coverage.
    let idx = do_index_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "full": true,
    }))
    .expect("index");
    let cov = &idx["coverage"];
    assert_eq!(
        cov["parse_incomplete"]["count"],
        json!(1),
        "index_codebase must report the corrupt file as parse_incomplete"
    );
    assert!(
        cov["caveat"]
            .as_str()
            .unwrap()
            .contains("NOT a completeness guarantee"),
        "the honesty caveat must be present"
    );
    let graph_path = idx["graph_path"].as_str().expect("graph_path").to_string();

    // index_status authoritative coverage.
    let status = run_index_status(&json!({ "graph_path": graph_path }));
    assert_eq!(status["status"], json!("ok"));
    assert_eq!(status["coverage"]["parse_incomplete"]["count"], json!(1));
    let listed = status["coverage"]["parse_incomplete"]["files"][0]["path"]
        .as_str()
        .expect("a listed parse_incomplete file");
    assert_eq!(listed, "src/bad.py");

    // query_graph(graph="missed") enumerates the misses (no Cypher needed).
    let missed = run_query_graph(&json!({ "graph_path": graph_path, "graph": "missed" }));
    assert_eq!(missed["graph"], json!("missed"));
    assert_eq!(missed["coverage"]["parse_incomplete"]["count"], json!(1));
}
