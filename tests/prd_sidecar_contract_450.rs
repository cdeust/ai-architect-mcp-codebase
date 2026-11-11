// Issue #450: the README documents the stage-5 sidecar
// (`stage-5.affected_symbols.json`) that `validate_prd_against_graph` reads.
// These tests hold the README to the code: the example it prints is fed to
// the validator as written, and each rule the section states is exercised.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{clustering, indexer, prd_validator, resolver};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const README: &str = include_str!("../README.md");

/// The marker the README puts on the line before the example's code fence.
const EXAMPLE_MARKER: &str = "<!-- sidecar-example:";

/// The JSON example of the README's sidecar section, parsed.
fn readme_example() -> Value {
    let after = README
        .split_once(EXAMPLE_MARKER)
        .expect("README carries the sidecar example marker")
        .1;
    let body = after
        .split_once("```json\n")
        .expect("a json fence follows the marker")
        .1
        .split_once("\n```")
        .expect("the fence is closed")
        .0;
    serde_json::from_str(body).expect("the README example is valid JSON")
}

/// A graph over one file that defines the `handle_request` the README
/// example modifies, and not the `RequestBudget` it adds.
fn fixture(root: &Path) -> GraphStore {
    let repo = root.join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/server.rs"),
        "pub fn handle_request(path: &str) -> usize {\n    path.len()\n}\n",
    )
    .unwrap();
    let graph = root.join("graph");
    indexer::index_codebase(&repo, &graph).expect("index");
    let store = GraphStore::open_or_create(&graph).unwrap();
    resolver::resolve_graph(&store).expect("resolve");
    clustering::cluster_graph(&store, 1.0).expect("cluster");
    GraphStore::open_or_create(&graph).unwrap()
}

fn write_sidecar(root: &Path, value: &Value) -> PathBuf {
    let path = root.join(prd_validator::AFFECTED_SYMBOLS_FILE);
    fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    path
}

fn write_prd(root: &Path, body: &str) -> PathBuf {
    let path = root.join("stage-5.prd.md");
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn the_readme_example_is_read_as_the_structured_contract() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(root.path());
    let prd = write_prd(root.path(), "Adds a timeout to `handle_request`.");
    let example = readme_example();
    let sidecar = write_sidecar(root.path(), &example);

    let report = prd_validator::validate_prd(&store, &prd, Some(&sidecar)).expect("validate");
    assert_eq!(report.extraction_mode, "structured");
    assert!(!report.contract_missing);
    assert_eq!(report.affected_symbol_count, 2);
    assert_eq!(report.scope_claim_count, 2);
    // `handle_request` resolves; the added `RequestBudget` is not scored.
    assert_eq!(report.summary.resolved_symbols, 1);
    assert_eq!(report.summary.hallucinated_symbols, 0);
    assert_eq!(report.summary.absent_symbols, 0);
    assert_eq!(report.validation_status, "ok");

    // The same missing symbol under `modify` is the critical finding.
    let mut fabricated = example.clone();
    fabricated["affected_symbols"][1]["change_kind"] = json!("modify");
    let sidecar = write_sidecar(root.path(), &fabricated);
    let report = prd_validator::validate_prd(&store, &prd, Some(&sidecar)).expect("validate");
    assert_eq!(report.summary.hallucinated_symbols, 1);
    assert_eq!(report.validation_status, "fail");
    assert!(report
        .findings
        .iter()
        .any(|f| f.severity == "critical"
            && f.symbol.as_deref() == Some("src/server.rs::RequestBudget")));

    // A file the index does not cover is unverifiable, not hallucinated.
    fabricated["affected_symbols"][1]["qualified_name"] = json!("web/app.php::render");
    let sidecar = write_sidecar(root.path(), &fabricated);
    let report = prd_validator::validate_prd(&store, &prd, Some(&sidecar)).expect("validate");
    assert_eq!(report.summary.hallucinated_symbols, 0);
    assert_eq!(report.summary.unverifiable_symbols, 1);
    assert_eq!(report.validation_status, "ok");
}

#[test]
fn defaults_and_empty_arrays_follow_the_documented_rules() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(root.path());
    let prd = write_prd(root.path(), "Mentions `NoSuchTypeAnywhere`.");

    // No `qualified_name`: ignored. No `change_kind`: `unknown`, not scored.
    let sidecar = write_sidecar(
        root.path(),
        &json!({
            "affected_symbols": [
                { "change_kind": "modify" },
                { "qualified_name": "src/server.rs::NoSuchTypeAnywhere" }
            ],
            "scope_claims": []
        }),
    );
    let report = prd_validator::validate_prd(&store, &prd, Some(&sidecar)).expect("validate");
    assert_eq!(report.affected_symbol_count, 1);
    assert_eq!(report.summary.hallucinated_symbols, 0);
    assert_eq!(report.validation_status, "ok");

    // Two empty arrays: the structured mode with zero claims, and the PRD
    // text is not scanned (its backticked token raises nothing).
    let sidecar = write_sidecar(
        root.path(),
        &json!({"affected_symbols": [], "scope_claims": []}),
    );
    let report = prd_validator::validate_prd(&store, &prd, Some(&sidecar)).expect("validate");
    assert_eq!(report.extraction_mode, "structured");
    assert!(!report.contract_missing);
    assert_eq!(report.affected_symbol_count, 0);
    assert!(report.findings.is_empty());
}

#[test]
fn no_file_a_missing_path_and_invalid_json_are_the_regex_fallback() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(root.path());
    let prd = write_prd(root.path(), "Rewrites `NoSuchTypeAnywhere`.");
    let missing = root.path().join("not-written.json");
    let invalid = root.path().join("invalid.json");
    fs::write(&invalid, "{ not json").unwrap();

    for sidecar in [None, Some(missing.as_path()), Some(invalid.as_path())] {
        let report = prd_validator::validate_prd(&store, &prd, sidecar).expect("validate");
        assert_eq!(report.extraction_mode, "regex_fallback", "{sidecar:?}");
        assert!(report.contract_missing, "{sidecar:?}");
        assert_eq!(report.scope_claim_count, 0);
        // The fabricated name is a warning; nothing is critical here.
        assert_eq!(report.summary.absent_symbols, 1, "{sidecar:?}");
        assert_eq!(report.summary.hallucinated_symbols, 0);
        assert_eq!(report.validation_status, "warning");
    }
}
