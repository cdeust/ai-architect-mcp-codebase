//! Stage 3a–3c handlers — `get_symbol`, `resolve_graph`, `cluster_graph`.
//!
//! `symbol_handlers.rs` had 0% line coverage (issue #169) while being the body
//! behind three advertised MCP tools. Two kinds of path are covered here and
//! both matter: the argument-validation arms, which are what a misconfigured
//! agent actually hits, and the graph-backed happy paths against a real
//! indexed fixture, which is the only way to prove the Cypher these functions
//! build returns anything at all.
//!
//! Every error arm asserts the REASON CODE, not merely that an error occurred:
//! `symbol_not_found` tells an agent to try a suggestion, `symbol_lookup_failed`
//! tells it the call was malformed, and swapping the two sends it down the
//! wrong recovery path.
use super::*;
use crate::indexer;
use std::fs;

const F_MAIN: &str = r#"
use crate::svc;
fn main() { let _ = svc::run_a(); }
"#;
const F_SVC: &str = r#"
use crate::helpers;
pub fn run_a() -> String { helpers::sanitize("a") }
"#;
const F_HELPERS: &str = r#"
pub fn sanitize(input: &str) -> String { input.trim().to_string() }
"#;

/// Builds an indexed + resolved + clustered fixture graph, returns its dir.
/// Mirrors `pagination_tests::build_fixture` — tempfile's random suffix rather
/// than `process::id()`, which collides under PID reuse (issue #25 audit).
fn build_fixture(tag: &str) -> std::path::PathBuf {
    let tmp_root = tempfile::Builder::new()
        .prefix(&format!("symbol_handlers_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&tmp_root);
    let src = tmp_root.join("fixture/src");
    fs::create_dir_all(&src).expect("create fixture src");
    fs::write(src.join("main.rs"), F_MAIN).unwrap();
    fs::write(src.join("svc.rs"), F_SVC).unwrap();
    fs::write(src.join("helpers.rs"), F_HELPERS).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = graph_store::GraphStore::open_or_create(&graph_dir).unwrap();
    resolver::resolve_graph(&store).expect("resolve");
    clustering::cluster_graph(&store, 1.0).expect("cluster");
    // Drop the handle so the read-path cache opens its own; the embedded DB is
    // single-writer and tests share a process.
    drop(store);
    graph_dir
}

/// An `graph_path` that is syntactically fine and absent on disk.
fn missing_graph_args(extra: &[(&str, Value)]) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "graph_path".to_string(),
        json!("/definitely/not/a/graph/xyzzy"),
    );
    for (k, v) in extra {
        map.insert(k.to_string(), v.clone());
    }
    Value::Object(map)
}

// ---------------------------------------------------------------------------
// get_symbol — argument validation
// ---------------------------------------------------------------------------

#[test]
fn get_symbol_rejects_non_object_arguments() {
    for bad in [json!(null), json!(3), json!("graph"), json!([])] {
        let err = do_get_symbol(&bad).expect_err("non-object arguments are rejected");
        assert_eq!(err, "arguments must be an object", "for {bad}");
    }
}

#[test]
fn get_symbol_names_the_missing_required_field() {
    // Two required fields, two distinct messages. "invalid arguments" would
    // leave a caller guessing which one it forgot.
    let err = do_get_symbol(&json!({"qualified_name": "a::b"})).expect_err("no graph_path");
    assert_eq!(err, "missing required field 'graph_path'");

    let err = do_get_symbol(&json!({"graph_path": "/tmp"})).expect_err("no qualified_name");
    assert_eq!(err, "missing required field 'qualified_name'");
}

#[test]
fn get_symbol_rejects_fields_of_the_wrong_type() {
    // `as_str` on a number yields None, which lands in the same arm as absent.
    // The contract is "a string is required", and a numeric graph_path must
    // not be silently coerced.
    let err = do_get_symbol(&json!({"graph_path": 7, "qualified_name": "a"}))
        .expect_err("numeric graph_path");
    assert_eq!(err, "missing required field 'graph_path'");
}

#[test]
fn get_symbol_reports_a_graph_path_that_does_not_exist() {
    let err = do_get_symbol(&missing_graph_args(&[("qualified_name", json!("a::b"))]))
        .expect_err("absent graph");
    assert!(
        err.starts_with("graph_path does not exist:"),
        "the message must distinguish 'no graph there' from 'symbol missing', got: {err}"
    );
    assert!(
        err.contains("xyzzy"),
        "and it must name the path, got: {err}"
    );
}

#[test]
fn run_get_symbol_wraps_failures_in_the_error_envelope() {
    // The `run_` wrapper is the wire boundary: an Err must become a structured
    // response, never a panic or a bare string.
    let out = run_get_symbol(&json!(null));
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "symbol_lookup_failed");
    assert_eq!(out["stage"], 3);
    assert_eq!(out["message"], "arguments must be an object");
}

// ---------------------------------------------------------------------------
// get_symbol — against a real graph
// ---------------------------------------------------------------------------

#[test]
fn get_symbol_returns_the_node_and_its_edges() {
    let graph = build_fixture("found");
    let out = run_get_symbol(&json!({
        "graph_path": graph.to_str().unwrap(),
        "qualified_name": "helpers.rs::sanitize",
    }));

    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["tool"], "get_symbol");
    assert!(!out["node"].is_null(), "the node must be found: {out}");
    assert_eq!(out["node"]["label"], "Function");
    // `sanitize` is called from svc.rs, so something points AT it. An empty
    // edges_in here would mean the incoming-edge Cypher matched nothing —
    // the failure mode that makes get_symbol look like it works while
    // answering "nothing depends on this".
    let edges_in = out["edges_in"].as_array().expect("edges_in is an array");
    assert!(!edges_in.is_empty(), "expected callers, got: {out}");
    assert!(out["edges_out"].is_array());
    // Both next steps must name the resolved symbol, or the suggestion is
    // not actionable.
    let steps = out["next_steps"].as_array().expect("next_steps");
    assert_eq!(steps.len(), 2);
    assert!(steps
        .iter()
        .all(|s| s.as_str().unwrap().contains("sanitize")));
}

#[test]
fn get_symbol_resolves_a_natural_src_prefixed_name() {
    // Three-layer lookup: an agent writes the path it sees on disk
    // (`src/helpers.rs::sanitize`); the graph stores `helpers.rs::sanitize`.
    let graph = build_fixture("prefix");
    let out = run_get_symbol(&json!({
        "graph_path": graph.to_str().unwrap(),
        "qualified_name": "src/helpers.rs::sanitize",
    }));
    assert_eq!(out["status"], "ok", "strip-path layer must resolve: {out}");
    assert!(!out["node"].is_null());
}

#[test]
fn get_symbol_reports_an_unknown_symbol_with_suggestions() {
    let graph = build_fixture("notfound");
    let out = run_get_symbol(&json!({
        "graph_path": graph.to_str().unwrap(),
        "qualified_name": "helpers.rs::saniti",
    }));

    // Not-found is a structured OK-shaped response with an error reason, not a
    // handler failure: the call was well-formed, the symbol was not there.
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "symbol_not_found");
    assert!(
        out["message"].as_str().unwrap().contains("saniti"),
        "the message must echo what was asked for: {out}"
    );
    assert!(
        out["did_you_mean"].is_array(),
        "the suggestion list must be present even when empty: {out}"
    );
}

#[test]
fn a_symbol_absent_everywhere_yields_no_sibling_claim() {
    // Negative assertion: with no sibling graphs configured the bridge is a
    // no-op and must not decorate the response with a foreign resolution.
    let graph = build_fixture("nosibling");
    let out = run_get_symbol(&json!({
        "graph_path": graph.to_str().unwrap(),
        "qualified_name": "totally::absent::symbol",
    }));
    assert_eq!(out["status"], "error");
    assert!(out.get("resolved_in").is_none(), "response: {out}");
    assert!(out.get("foreign_definitions").is_none(), "response: {out}");
}

// ---------------------------------------------------------------------------
// resolve_graph
// ---------------------------------------------------------------------------

#[test]
fn resolve_graph_validates_its_arguments() {
    assert_eq!(
        do_resolve_graph(&json!(null)).expect_err("non-object"),
        "arguments must be an object"
    );
    assert_eq!(
        do_resolve_graph(&json!({})).expect_err("no graph_path"),
        "missing required field 'graph_path'"
    );
    assert!(do_resolve_graph(&missing_graph_args(&[]))
        .expect_err("absent graph")
        .starts_with("graph_path does not exist:"));
}

#[test]
fn run_resolve_graph_wraps_failures_with_its_own_reason() {
    let out = run_resolve_graph(&json!(null));
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "resolve_failed");
}

#[test]
fn resolve_graph_reports_counts_and_a_rate() {
    let graph = build_fixture("resolve");
    let out = run_resolve_graph(&json!({"graph_path": graph.to_str().unwrap()}));

    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["tool"], "resolve_graph");
    for field in [
        "imports_resolved",
        "calls_resolved",
        "implements_resolved",
        "extends_resolved",
        "uses_resolved",
        "total_edges",
        "total_refs",
        "unresolved_count",
        "elapsed_ms",
    ] {
        assert!(out[field].is_number(), "{field} missing from: {out}");
    }
    // The rate is a formatted string, not a float, so a consumer sees a stable
    // two-decimal value rather than 0.6666666666666666.
    let rate = out["resolution_rate"].as_str().expect("rate is a string");
    assert_eq!(
        rate.split('.').nth(1).map(str::len),
        Some(2),
        "rate: {rate}"
    );
    assert!(rate.parse::<f64>().is_ok(), "rate must parse: {rate}");
    // The fixture has real cross-file calls, so resolution cannot be empty.
    assert!(out["total_edges"].as_u64().unwrap() > 0, "response: {out}");
}

#[test]
fn resolve_graph_adds_no_cross_repo_fields_without_siblings() {
    // Negative assertion: the bridge fields appear only when the caller asked
    // for siblings. A zero-valued `cross_repo_resolvable` would read as
    // "checked, found none" when nothing was checked.
    let graph = build_fixture("nobridge");
    let out = run_resolve_graph(&json!({"graph_path": graph.to_str().unwrap()}));
    assert!(out.get("cross_repo_resolvable").is_none(), "{out}");
    assert!(out.get("cross_repo_sample").is_none(), "{out}");
    assert!(out.get("sibling_graphs_skipped").is_none(), "{out}");
}

// ---------------------------------------------------------------------------
// cluster_graph
// ---------------------------------------------------------------------------

#[test]
fn cluster_graph_validates_its_arguments() {
    assert_eq!(
        do_cluster_graph(&json!(null)).expect_err("non-object"),
        "arguments must be an object"
    );
    assert_eq!(
        do_cluster_graph(&json!({})).expect_err("no graph_path"),
        "missing required field 'graph_path'"
    );
    assert!(do_cluster_graph(&missing_graph_args(&[]))
        .expect_err("absent graph")
        .starts_with("graph_path does not exist:"));
}

#[test]
fn run_cluster_graph_wraps_failures_with_its_own_reason() {
    let out = run_cluster_graph(&json!(null));
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "cluster_failed");
}

#[test]
fn cluster_graph_reports_communities_and_memberships() {
    let graph = build_fixture("cluster");
    let out = run_cluster_graph(&json!({"graph_path": graph.to_str().unwrap()}));

    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["tool"], "cluster_graph");
    assert!(out["community_count"].as_u64().unwrap() > 0, "{out}");
    assert!(out["process_count"].is_number());
    assert!(out["elapsed_ms"].is_number());
    assert!(out["total_memberships"].is_number());
    // Modularity is formatted to six places for the same reason the resolution
    // rate is formatted to two: a stable string beats a float's full expansion.
    let modularity = out["modularity"].as_str().expect("modularity is a string");
    assert_eq!(modularity.split('.').nth(1).map(str::len), Some(6));

    let clusters = out["clusters"].as_array().expect("clusters is an array");
    assert!(!clusters.is_empty(), "{out}");
    // Each membership carries both spellings of both fields; a consumer
    // reading either name must see the same value.
    for entry in clusters {
        assert_eq!(entry["qualified_name"], entry["qn"]);
        assert!(entry["community_id"].is_number() || entry["community_id"].is_string());
        assert!(entry.get("cluster_id").is_some());
    }
}

#[test]
fn cluster_graph_accepts_a_custom_resolution_parameter() {
    // `resolution_param` defaults to 1.0 when absent or non-numeric. Passing a
    // different gamma must be honoured rather than ignored, and passing
    // nonsense must fall back rather than fail the call.
    let graph = build_fixture("gamma");
    let path = graph.to_str().unwrap();

    let tuned = run_cluster_graph(&json!({"graph_path": path, "resolution_param": 0.5}));
    assert_eq!(tuned["status"], "ok", "response: {tuned}");

    let nonsense = run_cluster_graph(&json!({"graph_path": path, "resolution_param": "high"}));
    assert_eq!(
        nonsense["status"], "ok",
        "a non-numeric gamma falls back to the default: {nonsense}"
    );
}

// ---------------------------------------------------------------------------
// rel_table_triples — the table list the edge queries iterate
// ---------------------------------------------------------------------------

#[test]
fn the_relationship_table_list_is_well_formed() {
    // This list is mirrored from graph_store's private REL_TABLES. A duplicate
    // row would double-count an edge in get_symbol's output; an empty name
    // would build a Cypher query that cannot parse.
    let triples = rel_table_triples();
    assert!(!triples.is_empty());

    let mut seen = std::collections::HashSet::new();
    for (rel, from, to) in triples {
        assert!(!rel.is_empty() && !from.is_empty() && !to.is_empty());
        assert!(
            seen.insert(*rel),
            "duplicate relationship table would double-count edges: {rel}"
        );
        assert!(
            rel.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "table name is not a bare Cypher identifier: {rel}"
        );
    }
}

#[test]
fn every_triple_builds_a_query_the_store_accepts() {
    // This list is mirrored by hand from graph_store's private REL_TABLES, and
    // `collect_edge_rows` swallows query errors by design (`if let Ok`). So a
    // row naming a table or label that does not exist costs nothing at
    // runtime and silently drops that entire edge class from get_symbol's
    // answer — the failure mode where the tool looks healthy and under-reports.
    // Executing each triple's own query is the only way to catch it.
    let graph = build_fixture("triples");
    let store = graph_store::GraphStore::open_or_create(&graph).expect("open fixture");

    let mut rejected = Vec::new();
    for (rel, from_label, to_label) in rel_table_triples() {
        let cypher = format!(
            "MATCH (a:{from_label})-[r:{rel}]->(b:{to_label}) \
             RETURN '{rel}' AS rel_type, b.id AS target_id LIMIT 1"
        );
        if let Err(e) = store.execute_query(&cypher) {
            rejected.push(format!("{rel} ({from_label} -> {to_label}): {e}"));
        }
    }
    assert!(
        rejected.is_empty(),
        "{} relationship triple(s) build a query the store rejects, so their \
         edges are silently dropped:\n  {}",
        rejected.len(),
        rejected.join("\n  ")
    );
}
