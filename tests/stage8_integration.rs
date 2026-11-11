// stage8_integration — end-to-end tests for check_security_gates.
//
// Uses a fixture with auth-pattern symbols and verifies S1, S3, S5 gate
// behaviors plus the S2 info-skip mode.

use ai_architect_mcp::clustering;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use ai_architect_mcp::security_gates;
use std::fs;
mod common;
use common::TempDirExt;

// Fixture: one auth-pattern fn (`verify_token`) and a caller — Louvain puts
// them in the same community (direct Calls edge + same-file containment).
// The S1 bystander (`untested_helper`) lives in a SEPARATE file with no edge
// to main.rs: since stage-3c.md §2.4 includes File containment carriers,
// same-file symbols share a community by design, so "not in the auth
// community" requires a different file. Also includes a top-level pub fn
// for the S3 test.
const FIXTURE_MAIN: &str = r#"
fn main() {
    api_v1();
}

pub fn api_v1() -> String {
    let t = verify_token("abc");
    t.to_string()
}

fn verify_token(token: &str) -> &str {
    token
}
"#;

const FIXTURE_HELPER: &str = r#"
pub fn untested_helper() -> u32 {
    42
}
"#;

fn tmp(tag: &str) -> common::TestTempDir {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not (tag is kept as a
    // human-readable prefix for debuggability, not for uniqueness).
    tempfile::Builder::new()
        .prefix(&format!("stage8_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed()
}

fn build_fixture_graph(fixture_dir: &std::path::Path, graph_dir: &std::path::Path) -> GraphStore {
    fs::create_dir_all(fixture_dir.join("src")).unwrap();
    fs::write(fixture_dir.join("src/main.rs"), FIXTURE_MAIN).unwrap();
    fs::write(fixture_dir.join("src/helper.rs"), FIXTURE_HELPER).unwrap();
    let _ = indexer::index_codebase(&fixture_dir.join("src"), graph_dir).expect("index");
    let store = GraphStore::open_or_create(graph_dir).unwrap();
    let _ = resolver::resolve_graph(&store);
    let _ = clustering::cluster_graph(&store, 1.0);
    GraphStore::open_or_create(graph_dir).unwrap()
}

#[test]
fn test_s1_auth_critical_community_flag() {
    let root = tmp("s1");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    // Change `api_v1` — it calls verify_token so they should share a community.
    let changed = vec!["main.rs::api_v1".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    let s1: Vec<_> = report
        .flags
        .iter()
        .filter(|f| f.gate == "auth_critical_touch")
        .collect();
    assert!(
        !s1.is_empty(),
        "expected S1 flag; flags were: {:?}",
        report
            .flags
            .iter()
            .map(|f| (f.gate.clone(), f.severity.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(s1[0].severity, "critical");
    assert!(!report.gates_passed, "critical flag must fail the gate");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_s2_info_skip_mode() {
    let root = tmp("s2");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    let changed = vec!["helper.rs::untested_helper".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    let s2: Vec<_> = report
        .flags
        .iter()
        .filter(|f| f.gate == "unsafe_symbol")
        .collect();
    assert_eq!(
        s2.len(),
        1,
        "S2 must emit exactly one info flag per changed symbol"
    );
    assert_eq!(s2[0].severity, "info");
    assert!(s2[0].message.contains("is_unsafe"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_s3_public_api_warning() {
    let root = tmp("s3");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    // api_v1 is a crate-root `pub fn`.
    let changed = vec!["main.rs::api_v1".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    let s3: Vec<_> = report
        .flags
        .iter()
        .filter(|f| f.gate == "public_api_change")
        .collect();
    assert!(!s3.is_empty(), "expected S3 warning on crate-root pub fn");
    assert_eq!(s3[0].severity, "warning");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_s5_test_coverage_gap() {
    let root = tmp("s5");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    // No test entry points exist in the fixture — every symbol should trip S5.
    let changed = vec!["helper.rs::untested_helper".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    let s5: Vec<_> = report
        .flags
        .iter()
        .filter(|f| f.gate == "test_coverage_gap")
        .collect();
    assert!(
        !s5.is_empty(),
        "expected S5 warning when no test process reaches the symbol"
    );
    assert_eq!(s5[0].severity, "warning");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_gates_passed_reflects_severity() {
    let root = tmp("aggregate");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    // `untested_helper` is NOT in the verify_token community, so S1 should
    // not fire on it. Warnings-only must PASS the gate.
    let changed = vec!["helper.rs::untested_helper".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    assert_eq!(
        report.summary.critical_count,
        0,
        "expected no critical flags for bystander symbol; got {:?}",
        report
            .flags
            .iter()
            .map(|f| (f.gate.clone(), f.severity.clone()))
            .collect::<Vec<_>>()
    );
    assert!(report.gates_passed, "warnings-only must pass the gate");

    // api_v1 shares community with verify_token (S1 critical) => fails.
    let changed = vec!["main.rs::api_v1".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    assert!(report.summary.critical_count >= 1);
    assert!(!report.gates_passed);
    let _ = fs::remove_dir_all(&root);
}

/// The S2 skip flag for `symbol`.
fn unsafe_flag<'a>(
    report: &'a security_gates::SecurityReport,
    symbol: &str,
) -> &'a security_gates::SecurityFlag {
    report
        .flags
        .iter()
        .find(|f| f.gate == "unsafe_symbol" && f.symbol == symbol)
        .unwrap_or_else(|| panic!("no unsafe_symbol flag for {symbol}"))
}

/// Issue #450: the unsafe-code check decides no language, and says so with
/// the language of each symbol it skipped, in the flag and in the report's
/// `language_coverage`. A skip on a Python symbol used to read "the Rust
/// parser does not record is_unsafe".
#[test]
fn the_unsafe_check_states_the_languages_it_did_not_check() {
    let root = tmp("s2_languages");
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    fs::create_dir_all(fixture_dir.join("src")).unwrap();
    fs::write(
        fixture_dir.join("src/tool.py"),
        "def py_helper():\n    return 1\n",
    )
    .unwrap();
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    let changed = vec![
        "helper.rs::untested_helper".to_string(),
        "tool.py::py_helper".to_string(),
    ];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    for (symbol, language) in [
        ("helper.rs::untested_helper", "rust"),
        ("tool.py::py_helper", "python"),
    ] {
        let flag = unsafe_flag(&report, symbol);
        assert_eq!(flag.details["language"], language, "{}", flag.message);
        assert_eq!(flag.details["checked_languages"], serde_json::json!([]));
        assert_eq!(flag.details["skipped"], true);
        assert!(
            flag.message.contains(&format!("language: {language}")),
            "{}",
            flag.message
        );
    }

    let json = security_gates::report_to_json(&report, "", "", &graph_dir, &changed, "fixed");
    let coverage = &json["language_coverage"]["unsafe_symbol"];
    assert_eq!(coverage["checked_languages"], serde_json::json!([]));
    assert_eq!(
        coverage["not_checked_languages"],
        serde_json::json!(["python", "rust"])
    );
    assert_eq!(coverage["symbols_not_checked"], 2);
    assert_eq!(json["assessment_complete"], false);
}

/// Issue #450: a changed symbol outside the graph gets none of the checks.
/// It was reported as `info`; it is a `warning`, and when its file is in a
/// language no parser handles the flag says which.
#[test]
fn a_changed_symbol_outside_the_graph_is_a_warning_that_says_why() {
    let root = tmp("unresolved");
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    let changed = vec![
        "legacy/billing.php::charge".to_string(),
        "no_such_symbol_anywhere".to_string(),
    ];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    assert_eq!(report.flags.len(), 2, "only the two unresolved flags");
    assert_eq!(report.summary.warning_count, 2);
    assert_eq!(report.summary.info_count, 0);
    assert!(report.gates_passed, "a warning does not fail the gate");
    assert!(!report.assessment_complete());

    let php = &report.flags[0];
    assert_eq!(php.gate, "input_unresolved");
    assert_eq!(php.severity, "warning");
    assert_eq!(php.details["checks_run"], 0);
    let reason = php.details["unverifiable_reason"]
        .as_str()
        .expect("a file in an unparsed language has a reason");
    assert!(reason.contains(".php"), "{reason}");
    assert!(php.message.contains("outside the graph"), "{}", php.message);

    let bare = &report.flags[1];
    assert_eq!(bare.severity, "warning");
    assert!(bare.details["unverifiable_reason"].is_null());
    assert!(
        bare.message.contains("none of the checks ran on it"),
        "{}",
        bare.message
    );

    // No changed symbol reached the unsafe check: the block says none did.
    let json = security_gates::report_to_json(&report, "", "", &graph_dir, &changed, "fixed");
    let coverage = &json["language_coverage"]["unsafe_symbol"];
    assert_eq!(coverage["symbols_not_checked"], 0);
    assert_eq!(coverage["not_checked_languages"], serde_json::json!([]));
}
