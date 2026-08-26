//! Stage 3d/3e handlers — `search_codebase`, `get_context`, `index_history`.
//!
//! `search_context_handlers.rs` sat at 43.83% line coverage (issue #169): the
//! paging contract was already exercised by `pagination_tests`, but the
//! argument-validation arms, the token-surface shaping (`detail` / `format`),
//! the not-found path with suggestions, and `index_history` in its entirety
//! were not.
//!
//! Where `pagination_tests` proves the cursor is correct, this suite proves
//! the surrounding decisions are: what a malformed call is told, which shape
//! comes back for each projection, and that opt-in sections stay absent when
//! nobody asked for them.
use super::*;
use crate::indexer;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const F_MAIN: &str = r#"
use crate::svc;
fn main() { let _ = svc::run_a(); }
"#;
const F_SVC: &str = r#"
use crate::helpers;
pub fn run_a() -> String { helpers::sanitize("a") }
pub fn run_b() -> String { helpers::sanitize("b") }
"#;
const F_HELPERS: &str = r#"
pub fn sanitize(input: &str) -> String { input.trim().to_string() }
pub struct Sanitizer { pub strict: bool }
"#;

struct Fixture {
    // Held for its Drop side effect (cleans the backing tempdir on a passing
    // test, preserves it on a panicking one) — never read directly.
    _guard: crate::test_support::TestTempDir,
    graph: PathBuf,
    src: PathBuf,
}

fn build_fixture(tag: &str) -> Fixture {
    use crate::test_support::TempDirExt;
    let tmp_root = tempfile::Builder::new()
        .prefix(&format!("search_ctx_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);
    let src = tmp_root.join("fixture/src");
    fs::create_dir_all(&src).expect("create fixture src");
    fs::write(src.join("main.rs"), F_MAIN).unwrap();
    fs::write(src.join("svc.rs"), F_SVC).unwrap();
    fs::write(src.join("helpers.rs"), F_HELPERS).unwrap();

    let graph = tmp_root.join("graph");
    indexer::index_codebase(&src, &graph).expect("index");
    let store = graph_store::GraphStore::open_or_create(&graph).unwrap();
    crate::resolver::resolve_graph(&store).expect("resolve");
    crate::clustering::cluster_graph(&store, 1.0).expect("cluster");
    // build_search_index writes `<output_dir>/search_index`, and the handler
    // resolves it as a sibling of the graph dir — so the output dir is the
    // graph's PARENT, not the graph itself.
    search::build_search_index(&store, &tmp_root, &src).expect("build search index");
    drop(store);
    Fixture {
        _guard: tmp_root,
        graph,
        src,
    }
}

fn graph_arg(f: &Fixture) -> &str {
    f.graph.to_str().expect("utf-8 path")
}

// ---------------------------------------------------------------------------
// search_codebase — argument validation
// ---------------------------------------------------------------------------

#[test]
fn search_rejects_non_object_arguments() {
    assert_eq!(
        do_search_codebase(&json!("query")).expect_err("non-object"),
        "arguments must be an object"
    );
}

#[test]
fn search_names_the_missing_required_field() {
    assert_eq!(
        do_search_codebase(&json!({"query": "x"})).expect_err("no graph_path"),
        "missing required field 'graph_path'"
    );
    assert_eq!(
        do_search_codebase(&json!({"graph_path": "/tmp"})).expect_err("no query"),
        "missing required field 'query'"
    );
}

#[test]
fn search_reports_an_absent_graph() {
    let err = do_search_codebase(&json!({
        "graph_path": "/tmp/ap-no-graph-xyzzy", "query": "x",
    }))
    .expect_err("absent graph");
    assert!(err.starts_with("graph_path does not exist:"), "{err}");
}

#[test]
fn run_search_codebase_wraps_failures_in_the_error_envelope() {
    let out = run_search_codebase(&json!(null));
    assert_eq!(out["stage"], 3);
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "search_failed");
}

// ---------------------------------------------------------------------------
// search_codebase — the response shape
// ---------------------------------------------------------------------------

#[test]
fn search_returns_ranked_hits_with_their_paging_metadata() {
    let f = build_fixture("search_ok");
    let out = run_search_codebase(&json!({"graph_path": graph_arg(&f), "query": "sanitize"}));

    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["tool"], "search_codebase");
    assert_eq!(out["query"], "sanitize");
    assert!(out["result_count"].as_u64().unwrap() > 0, "{out}");
    assert!(out["total_count"].is_number());
    assert_eq!(out["offset"], 0);
    assert!(out["truncated"].is_boolean());
    assert!(out["elapsed_ms"].is_number());
    assert!(out["by_process"].is_array());

    // Scores are formatted to four places so a consumer sees a stable string
    // rather than a float's full expansion.
    let results = out["results"].as_array().expect("results is an array");
    let score = results[0]["score"].as_str().expect("score is a string");
    assert_eq!(
        score.split('.').nth(1).map(str::len),
        Some(4),
        "score: {score}"
    );
}

#[test]
fn a_hit_earns_actionable_next_steps_and_an_empty_page_does_not() {
    // The suggestion is gated on a non-empty page: proposing `get_context` on
    // nothing sends an agent to make a call that cannot succeed.
    let f = build_fixture("nextsteps");
    let hit = run_search_codebase(&json!({"graph_path": graph_arg(&f), "query": "sanitize"}));
    let steps = hit["next_steps"].as_array().expect("next_steps on a hit");
    assert_eq!(steps.len(), 2);
    assert!(steps[0].as_str().unwrap().contains("get_context"));

    let miss = run_search_codebase(&json!({
        "graph_path": graph_arg(&f),
        "query": "zzzz_nothing_matches_this_zzzz",
    }));
    assert_eq!(miss["status"], "ok", "an empty result set is not an error");
    assert_eq!(miss["result_count"], 0, "{miss}");
    assert!(
        miss.get("next_steps").is_none(),
        "no hit, no suggestion: {miss}"
    );
}

#[test]
fn the_ids_projection_returns_bare_qualified_names() {
    // issue #56 token surface: `detail:"ids"` is the cheap wide sweep. Getting
    // objects back here defeats the point of asking for it.
    let f = build_fixture("ids");
    let out = run_search_codebase(&json!({
        "graph_path": graph_arg(&f), "query": "sanitize", "detail": "ids",
    }));
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(out["detail"], "ids");
    let results = out["results"].as_array().expect("results");
    assert!(!results.is_empty(), "{out}");
    assert!(
        results.iter().all(|r| r.is_string()),
        "ids must be bare strings, got: {out}"
    );
}

#[test]
fn the_tabular_projection_declares_its_columns_once() {
    // Rows as arrays only make sense alongside the column order they follow;
    // rows without `columns` are unreadable.
    let f = build_fixture("tabular");
    let out = run_search_codebase(&json!({
        "graph_path": graph_arg(&f), "query": "sanitize", "format": "tabular",
    }));
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(out["format"], "tabular");
    let columns = out["columns"].as_array().expect("tabular declares columns");
    assert_eq!(
        columns.len(),
        SEARCH_COLUMNS.len(),
        "columns must match the declared projection: {out}"
    );
    for row in out["results"].as_array().expect("results") {
        let cells = row.as_array().expect("a tabular row is an array");
        assert_eq!(cells.len(), columns.len(), "ragged row: {row}");
        // Every cell must be scalar — `processes` is deliberately excluded
        // from this projection for exactly that reason.
        assert!(
            cells.iter().all(|c| !c.is_array() && !c.is_object()),
            "a tabular cell must be scalar: {row}"
        );
    }
}

#[test]
fn the_default_projection_returns_full_objects() {
    let f = build_fixture("default_view");
    let out = run_search_codebase(&json!({"graph_path": graph_arg(&f), "query": "sanitize"}));
    assert_eq!(out["detail"], "full");
    assert_eq!(out["format"], "json");
    assert!(
        out.get("columns").is_none(),
        "json format declares no columns"
    );
    let first = &out["results"].as_array().expect("results")[0];
    assert!(first.is_object(), "{first}");
    for field in ["qualified_name", "name", "kind", "file_path", "score"] {
        assert!(first.get(field).is_some(), "{field} missing from {first}");
    }
}

#[test]
fn a_label_filter_narrows_the_result_set() {
    let f = build_fixture("labelfilter");
    let unfiltered = run_search_codebase(&json!({
        "graph_path": graph_arg(&f), "query": "sanitize",
    }));
    let filtered = run_search_codebase(&json!({
        "graph_path": graph_arg(&f), "query": "sanitize", "label_filter": "Struct",
    }));
    assert_eq!(filtered["status"], "ok", "{filtered}");
    for row in filtered["results"].as_array().expect("results") {
        assert_eq!(row["kind"], "Struct", "filter leaked a non-Struct: {row}");
    }
    assert!(
        filtered["results"].as_array().unwrap().len()
            <= unfiltered["results"].as_array().unwrap().len(),
        "a filter must not widen the set"
    );
}

#[test]
fn search_adds_no_foreign_sections_without_siblings() {
    // Negative assertion: the cross-repo bridge is opt-in. An empty
    // `foreign_results` would read as "searched siblings, found nothing".
    let f = build_fixture("nofederation");
    let out = run_search_codebase(&json!({"graph_path": graph_arg(&f), "query": "sanitize"}));
    assert!(out.get("foreign_results").is_none(), "{out}");
    assert!(out.get("foreign_results_total").is_none(), "{out}");
    assert!(out.get("sibling_graphs_skipped").is_none(), "{out}");
}

#[test]
fn the_declared_search_columns_are_scalar_and_unique() {
    // `processes` is excluded on purpose (a tabular cell must be scalar); a
    // duplicate column would misalign every row against its header.
    let mut seen = std::collections::HashSet::new();
    for col in SEARCH_COLUMNS {
        assert!(seen.insert(*col), "duplicate column: {col}");
    }
    assert!(
        !SEARCH_COLUMNS.contains(&"processes"),
        "processes is an array and cannot be a tabular cell"
    );
    assert!(SEARCH_COLUMNS.contains(&"qualified_name"), "the key column");
}

// ---------------------------------------------------------------------------
// get_context
// ---------------------------------------------------------------------------

#[test]
fn get_context_validates_its_arguments() {
    assert_eq!(
        do_get_context(&json!(1)).expect_err("non-object"),
        "arguments must be an object"
    );
    assert_eq!(
        do_get_context(&json!({"qualified_name": "a"})).expect_err("no graph_path"),
        "missing required field 'graph_path'"
    );
    assert_eq!(
        do_get_context(&json!({"graph_path": "/tmp"})).expect_err("no qualified_name"),
        "missing required field 'qualified_name'"
    );
    let err = do_get_context(&json!({
        "graph_path": "/tmp/ap-no-graph-xyzzy", "qualified_name": "a",
    }))
    .expect_err("absent graph");
    assert!(err.starts_with("graph_path does not exist:"), "{err}");
}

#[test]
fn run_get_context_wraps_failures_in_the_error_envelope() {
    let out = run_get_context(&json!(null));
    assert_eq!(out["stage"], 3);
    assert_eq!(out["reason"], "context_failed");
}

#[test]
fn get_context_returns_the_relationship_view_of_a_symbol() {
    let f = build_fixture("context_ok");
    let out = run_get_context(&json!({
        "graph_path": graph_arg(&f),
        "qualified_name": "helpers.rs::sanitize",
    }));
    assert_eq!(out["status"], "ok", "response: {out}");
    // The 360-degree view: symbol identity, every relationship direction, and
    // the community/process placement an agent uses to judge blast radius.
    assert_eq!(out["symbol"]["name"], "sanitize", "{out}");
    let rel = &out["relationships"];
    for direction in [
        "imports",
        "imported_by",
        "calls",
        "called_by",
        "implements",
        "implemented_by",
        "uses",
        "used_by",
    ] {
        assert!(
            rel[direction].is_array(),
            "{direction} must always be present, even when empty: {out}"
        );
    }
    // `sanitize` is called from svc.rs twice, so `called_by` must not be empty
    // — an empty one here is the "nothing depends on this" false answer.
    assert!(
        !rel["called_by"].as_array().unwrap().is_empty(),
        "expected callers: {out}"
    );
}

#[test]
fn an_unknown_symbol_is_a_structured_not_found_not_a_handler_failure() {
    // The distinction matters: `symbol_not_found` carries suggestions an agent
    // can retry with, while `context_failed` means the call itself was wrong.
    let f = build_fixture("context_notfound");
    let out = run_get_context(&json!({
        "graph_path": graph_arg(&f),
        "qualified_name": "helpers.rs::saniti",
    }));
    assert_eq!(out["status"], "error");
    assert_eq!(
        out["reason"], "symbol_not_found",
        "must not be wrapped as context_failed: {out}"
    );
    assert!(out["did_you_mean"].is_array(), "{out}");
    assert!(out["message"].as_str().unwrap().contains("saniti"), "{out}");
}

// ---------------------------------------------------------------------------
// index_history
// ---------------------------------------------------------------------------

#[test]
fn index_history_validates_its_arguments() {
    assert_eq!(
        do_index_history(&json!(null)).expect_err("non-object"),
        "arguments must be an object"
    );
    assert_eq!(
        do_index_history(&json!({"codebase_path": "/tmp"})).expect_err("no graph_path"),
        "missing required field 'graph_path'"
    );
    assert_eq!(
        do_index_history(&json!({"graph_path": "/tmp"})).expect_err("no codebase_path"),
        "missing required field 'codebase_path'"
    );
}

#[test]
fn index_history_distinguishes_which_path_is_missing() {
    // Two paths, two messages. "a path does not exist" would leave a caller
    // checking both.
    let f = build_fixture("history_paths");
    let err = do_index_history(&json!({
        "graph_path": "/tmp/ap-no-graph-xyzzy", "codebase_path": "/tmp",
    }))
    .expect_err("absent graph");
    assert!(err.starts_with("graph_path does not exist:"), "{err}");

    let err = do_index_history(&json!({
        "graph_path": graph_arg(&f), "codebase_path": "/tmp/ap-no-codebase-xyzzy",
    }))
    .expect_err("absent codebase");
    assert!(err.starts_with("codebase_path does not exist:"), "{err}");
}

#[test]
fn run_index_history_wraps_failures_in_the_error_envelope() {
    let out = run_index_history(&json!(null));
    assert_eq!(out["stage"], 3);
    assert_eq!(out["reason"], "index_history_failed");
}

#[test]
fn index_history_folds_commits_into_the_graph() {
    let f = build_fixture("history_ok");
    // A real repository, because index_history shells out to git; a fake
    // directory would only prove the argument checks run.
    let repo = f.src.parent().expect("fixture root");
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("run git");
        assert!(ok.status.success(), "git {args:?}: {ok:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "first"]);
    fs::write(
        f.src.join("helpers.rs"),
        format!("{F_HELPERS}\n// touched\n"),
    )
    .unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "second"]);

    let out = run_index_history(&json!({
        "graph_path": graph_arg(&f),
        "codebase_path": repo.to_str().unwrap(),
    }));
    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["tool"], "index_history");
    assert!(
        out["commits"].as_u64().unwrap() >= 2,
        "both commits must be indexed: {out}"
    );
    for field in ["versions", "commit_edges", "version_edges"] {
        assert!(out[field].is_number(), "{field} missing: {out}");
    }
}

#[test]
fn index_history_honours_a_commit_cap() {
    let f = build_fixture("history_cap");
    let repo = f.src.parent().expect("fixture root");
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("run git");
    };
    git(&["init", "-q"]);
    for n in 0..3 {
        fs::write(f.src.join("helpers.rs"), format!("{F_HELPERS}\n// {n}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "c"]);
    }

    let out = run_index_history(&json!({
        "graph_path": graph_arg(&f),
        "codebase_path": repo.to_str().unwrap(),
        "max_commits": 1,
    }));
    assert_eq!(out["status"], "ok", "response: {out}");
    assert!(
        out["commits"].as_u64().unwrap() <= 1,
        "max_commits must bound the walk, got: {out}"
    );
}
