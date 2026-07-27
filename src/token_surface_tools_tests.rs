// Issue #56 — the detail/format token surface, exercised end to end through
// the real handlers.
use super::*;
use std::fs;

fn indexed_graph() -> (tempfile::TempDir, String) {
    let tmp = tempfile::Builder::new()
        .prefix("token_surface_tools_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("src")).expect("mk src");
    fs::write(
        repo.join("src/a.py"),
        "def process_one():\n    return 1\n\ndef process_two():\n    return 2\n",
    )
    .expect("write a");
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).expect("mk out");
    let idx = do_index_codebase(&json!({
        "path": repo.to_string_lossy(),
        "output_dir": out.to_string_lossy(),
        "full": true,
    }))
    .expect("index");
    let graph = idx["graph_path"].as_str().expect("graph_path").to_string();
    (tmp, graph)
}

#[test]
fn search_ids_mode_returns_bare_identifiers() {
    let (_tmp, graph) = indexed_graph();
    let resp = run_search_codebase(&json!({
        "graph_path": graph, "query": "process", "detail": "ids"
    }));
    assert_eq!(resp["detail"], json!("ids"));
    let results = resp["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "should find process_* symbols");
    // Every element is a bare qualified-name string, not an object.
    assert!(
        results.iter().all(|v| v.is_string()),
        "ids mode must return bare identifiers, got {results:?}"
    );
}

#[test]
fn search_tabular_mode_declares_columns_and_streams_rows() {
    let (_tmp, graph) = indexed_graph();
    let resp = run_search_codebase(&json!({
        "graph_path": graph, "query": "process", "format": "tabular"
    }));
    // Guard the tabular contract behind a success check: a failed search
    // (e.g. the store could not be opened because the disk is full)
    // returns `{status:"error", message:...}` with no `format` field, and
    // asserting on the absent field would mis-report a resource failure as
    // a tabular-mode regression (issue #143).
    assert_ne!(
        resp["status"],
        json!("error"),
        "search_codebase failed before the tabular contract could be checked: {}",
        resp["message"]
    );
    assert_eq!(resp["format"], json!("tabular"));
    assert!(
        resp["columns"].is_array(),
        "tabular must declare a columns header"
    );
    let results = resp["results"].as_array().expect("results array");
    assert!(
        results.iter().all(|row| row.is_array()),
        "tabular rows must be arrays of cells, got {results:?}"
    );
}

#[test]
fn query_graph_tabular_drops_the_human_result_string() {
    let (_tmp, graph) = indexed_graph();
    // Default keeps the human `result` table.
    let default = run_query_graph(&json!({
        "graph_path": graph, "query": "MATCH (f:Function) RETURN f.qualified_name"
    }));
    assert!(
        default.get("result").is_some(),
        "default query_graph keeps the result string"
    );
    // Tabular drops it (rows+columns already carry the data).
    let tabular = run_query_graph(&json!({
        "graph_path": graph,
        "query": "MATCH (f:Function) RETURN f.qualified_name",
        "format": "tabular"
    }));
    assert_eq!(tabular["format"], json!("tabular"));
    assert!(
        tabular.get("result").is_none(),
        "tabular query_graph must omit the redundant human result string"
    );
    assert!(tabular["rows"].is_array());
    // Serialized, tabular must be smaller than the default for the same query.
    let d = serde_json::to_string(&default).unwrap().len();
    let t = serde_json::to_string(&tabular).unwrap().len();
    assert!(t < d, "tabular ({t}) must be smaller than default ({d})");
}

#[test]
fn query_graph_ids_mode_collapses_to_first_column() {
    let (_tmp, graph) = indexed_graph();
    let resp = run_query_graph(&json!({
        "graph_path": graph,
        "query": "MATCH (f:Function) RETURN f.qualified_name, f.name",
        "detail": "ids"
    }));
    assert_eq!(resp["detail"], json!("ids"));
    let rows = resp["rows"].as_array().expect("rows array");
    assert!(
        rows.iter().all(|v| v.is_string()),
        "ids mode must collapse rows to the first column's bare values, got {rows:?}"
    );
}
