//! Stage 4/6/8/9 handlers — `prepare_prd_input`, `validate_prd_against_graph`,
//! `check_security_gates`, `verify_semantic_diff`.
//!
//! `prd_handlers.rs` had 0% line coverage (issue #169) while being the body
//! behind four advertised MCP tools. These handlers are almost entirely
//! boundary code: they decide what a caller is allowed to pass, translate a
//! failure into a reason code an agent routes on, and decide whether an
//! artifact gets written to disk at all. Every one of those decisions is a
//! path, and none of them was exercised.
//!
//! Two things are asserted throughout rather than merely "an error happened":
//! the reason code (an agent picks its next move from it) and, for the
//! artifact-writing helpers, whether a file appeared — the silent-write
//! decision is invisible in the response body otherwise.
use super::*;
use crate::graph_store;
use crate::indexer;
use std::fs;

const F_MAIN: &str = r#"
use crate::svc;
fn main() { let _ = svc::run_a(); }
"#;
const F_SVC: &str = r#"
pub fn run_a() -> String { String::from("a") }
"#;

fn build_fixture(tag: &str) -> (crate::test_support::TestTempDir, PathBuf) {
    use crate::test_support::TempDirExt;
    let tmp_root = tempfile::Builder::new()
        .prefix(&format!("prd_handlers_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);
    let src = tmp_root.join("fixture/src");
    fs::create_dir_all(&src).expect("create fixture src");
    fs::write(src.join("main.rs"), F_MAIN).unwrap();
    fs::write(src.join("svc.rs"), F_SVC).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = graph_store::GraphStore::open_or_create(&graph_dir).unwrap();
    crate::resolver::resolve_graph(&store).expect("resolve");
    crate::clustering::cluster_graph(&store, 1.0).expect("cluster");
    drop(store);
    (tmp_root, graph_dir)
}

fn tmp_dir(tag: &str) -> crate::test_support::TestTempDir {
    use crate::test_support::TempDirExt;
    tempfile::Builder::new()
        .prefix(&format!("prd_handlers_{tag}_"))
        .tempdir()
        .expect("tempdir")
        .keep_managed()
}

// ---------------------------------------------------------------------------
// stage4_error_response — the reason-code classifier
// ---------------------------------------------------------------------------

#[test]
fn stage4_classifies_a_stage_2_gate_failure() {
    // An agent routes on these: `stage_2_not_verified` means "go finish the
    // verification session", which is a different action from every other
    // failure this tool can produce.
    let out = stage4_error_response("stage_2_not_verified: finding F-1 has no receipt");
    assert_eq!(out["reason"], "stage_2_not_verified");
    assert_eq!(out["stage"], 4);
    assert_eq!(out["status"], "error");
    assert!(out["message"].as_str().unwrap().contains("F-1"));
}

#[test]
fn stage4_collapses_all_three_stage_1_failures_to_one_reason() {
    // Missing, unreadable and corrupt are three causes with one remedy — go
    // run stage 1 again — so they deliberately share a reason code. Asserting
    // all three keeps a later edit from splitting one out silently.
    for msg in [
        "stage_1_refined_missing: no such file",
        "stage_1_refined_unreadable: permission denied",
        "stage_1_refined_corrupt: unexpected token",
    ] {
        assert_eq!(
            stage4_error_response(msg)["reason"],
            "stage_1_refined_missing",
            "for {msg}"
        );
    }
}

#[test]
fn stage4_falls_back_to_the_generic_reason() {
    let out = stage4_error_response("arguments must be an object");
    assert_eq!(out["reason"], "prepare_prd_input_failed");
    assert_eq!(out["message"], "arguments must be an object");
}

#[test]
fn stage4_matches_on_a_prefix_not_a_substring() {
    // A message that merely MENTIONS a stage-2 failure must not be reclassified
    // as one — the classifier reads the prefix, and this pins that.
    let out = stage4_error_response("graph read failed while checking stage_2_not_verified");
    assert_eq!(out["reason"], "prepare_prd_input_failed");
}

// ---------------------------------------------------------------------------
// prepare_prd_input — argument validation
// ---------------------------------------------------------------------------

#[test]
fn prepare_prd_input_rejects_non_object_arguments() {
    assert_eq!(
        do_prepare_prd_input(&json!([])).expect_err("non-object"),
        "arguments must be an object"
    );
}

#[test]
fn prepare_prd_input_requires_one_of_the_two_input_modes() {
    // finding mode (finding_id) or feature mode (feature_description). With
    // neither there is nothing to ground, and the message must say so rather
    // than failing later on a missing file.
    let err = do_prepare_prd_input(&json!({"output_dir": "/tmp", "graph_path": "/tmp"}))
        .expect_err("no mode selected");
    assert!(err.contains("finding_id"), "{err}");
    assert!(err.contains("feature_description"), "{err}");
}

#[test]
fn prepare_prd_input_rejects_unsafe_ids() {
    // Path segments. A `..` or a separator here escapes the run directory.
    for (field, value) in [("run_id", "../escape"), ("finding_id", "a/b")] {
        let args = json!({
            field: value,
            "feature_description": "x",
            "output_dir": "/tmp/out",
            "graph_path": "/tmp/graph",
        });
        let err = do_prepare_prd_input(&args).expect_err("unsafe id must be rejected");
        assert!(
            err.starts_with(&format!("unsafe {field}")),
            "for {field}={value}, got: {err}"
        );
    }
}

#[test]
fn prepare_prd_input_requires_absolute_paths() {
    let base = |extra: Value| -> Value {
        let mut m = serde_json::Map::new();
        m.insert("feature_description".into(), json!("x"));
        for (k, v) in extra.as_object().unwrap() {
            m.insert(k.clone(), v.clone());
        }
        Value::Object(m)
    };
    let err = do_prepare_prd_input(&base(json!({"output_dir": "relative/out"})))
        .expect_err("relative output_dir");
    assert!(
        err.starts_with("output_dir must be an absolute path"),
        "{err}"
    );

    let err = do_prepare_prd_input(&base(
        json!({"output_dir": "/tmp/out", "graph_path": "relative/graph"}),
    ))
    .expect_err("relative graph_path");
    assert!(
        err.starts_with("graph_path must be an absolute path"),
        "{err}"
    );
}

#[test]
fn prepare_prd_input_names_the_missing_required_path() {
    let err =
        do_prepare_prd_input(&json!({"feature_description": "x"})).expect_err("no output_dir");
    assert_eq!(err, "missing required field 'output_dir'");

    let err = do_prepare_prd_input(&json!({"feature_description": "x", "output_dir": "/tmp/o"}))
        .expect_err("no graph_path");
    assert_eq!(err, "missing required field 'graph_path'");
}

#[test]
fn prepare_prd_input_reports_an_absent_graph() {
    let err = do_prepare_prd_input(&json!({
        "feature_description": "x",
        "output_dir": "/tmp/out",
        "graph_path": "/tmp/definitely-not-a-graph-xyzzy",
    }))
    .expect_err("absent graph");
    assert!(err.starts_with("graph_path does not exist:"), "{err}");
}

#[test]
fn run_prepare_prd_input_wraps_failures_in_the_stage_4_envelope() {
    let out = run_prepare_prd_input(&json!(null));
    assert_eq!(out["stage"], 4);
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "prepare_prd_input_failed");
}

#[test]
fn prepare_prd_input_in_feature_mode_grounds_against_the_graph() {
    let (_graph_guard, graph) = build_fixture("feature");
    let out_dir = tmp_dir("feature_out");
    let out = run_prepare_prd_input(&json!({
        "feature_description": "change run_a to return a number",
        "output_dir": out_dir.to_str().unwrap(),
        "graph_path": graph.to_str().unwrap(),
    }));

    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["mode"], "feature", "no finding_id means feature mode");
    assert_eq!(out["run_id"], "adhoc", "run_id defaults for ad-hoc calls");
    assert!(out["finding_id"].is_null());
    assert!(out["prd_context"].is_object() || out["prd_context"].is_array());
    for field in [
        "matched_symbol_count",
        "candidate_symbol_count",
        "impacted_community_count",
        "impacted_process_count",
    ] {
        assert!(out[field].is_number(), "{field} missing from {out}");
    }
    // The artifact is the deliverable — the response promises a path, so a
    // file must actually be there.
    let artifact = out["artifact_path"].as_str().expect("artifact_path");
    assert!(Path::new(artifact).exists(), "no artifact at {artifact}");
}

// ---------------------------------------------------------------------------
// verify_semantic_diff
// ---------------------------------------------------------------------------

#[test]
fn verify_semantic_diff_validates_its_arguments() {
    assert_eq!(
        do_verify_semantic_diff(&json!(7)).expect_err("non-object"),
        "arguments must be an object"
    );
    assert_eq!(
        do_verify_semantic_diff(&json!({})).expect_err("no before"),
        "missing required field 'before_graph_path'"
    );
    assert_eq!(
        do_verify_semantic_diff(&json!({"before_graph_path": "/a"})).expect_err("no after"),
        "missing required field 'after_graph_path'"
    );
    // The optional report_path is validated too, or a relative path would be
    // resolved against whatever directory the server happens to run in.
    let err = do_verify_semantic_diff(&json!({
        "before_graph_path": "/a", "after_graph_path": "/b", "report_path": "rel.json",
    }))
    .expect_err("relative report_path");
    assert!(
        err.starts_with("report_path must be an absolute path"),
        "{err}"
    );
}

#[test]
fn run_verify_semantic_diff_says_which_side_of_the_comparison_is_missing() {
    // Two sides, two distinct reasons: an agent that just built the "after"
    // graph needs to know which end it failed to supply. One generic
    // "diff failed" would send it to re-index both.
    let (_present_guard, present) = build_fixture("diff_side");
    let good = present.to_str().unwrap();

    let missing_before = run_verify_semantic_diff(&json!({
        "before_graph_path": "/tmp/ap-absent-before-xyzzy",
        "after_graph_path": good,
    }));
    assert_eq!(missing_before["stage"], 9);
    assert_eq!(
        missing_before["reason"], "before_graph_path_missing",
        "response: {missing_before}"
    );

    let missing_after = run_verify_semantic_diff(&json!({
        "before_graph_path": good,
        "after_graph_path": "/tmp/ap-absent-after-xyzzy",
    }));
    assert_eq!(
        missing_after["reason"], "after_graph_path_missing",
        "response: {missing_after}"
    );
}

#[test]
fn run_verify_semantic_diff_falls_back_to_the_generic_reason() {
    let out = run_verify_semantic_diff(&json!(null));
    assert_eq!(out["stage"], 9);
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "verify_semantic_diff_failed");
}

#[test]
fn verify_semantic_diff_compares_two_real_graphs() {
    let (_before_guard, before) = build_fixture("diff_before");
    let (_after_guard, after) = build_fixture("diff_after");
    let report_dir = tmp_dir("diff_report");
    let report_path = report_dir.join("report.json");

    let out = run_verify_semantic_diff(&json!({
        "before_graph_path": before.to_str().unwrap(),
        "after_graph_path": after.to_str().unwrap(),
        "report_path": report_path.to_str().unwrap(),
    }));

    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["tool"], "verify_semantic_diff");
    for field in [
        "nodes_added",
        "nodes_removed",
        "edges_added",
        "edges_removed",
        "dangling_references",
        "new_unresolved_delta",
        "new_cycles",
    ] {
        assert!(out["summary"][field].is_number(), "{field} missing: {out}");
    }
    assert!(out["verdict"].is_string(), "{out}");
    // report_path was supplied, so the report must be on disk AND echoed back.
    assert_eq!(out["report_path"], json!(report_path.to_string_lossy()));
    assert!(report_path.exists(), "report was promised but not written");
}

#[test]
fn verify_semantic_diff_writes_no_report_when_none_was_asked_for() {
    // Negative assertion: the write is opt-in. A tool that always writes
    // leaves files in a caller's tree that the caller never requested.
    let (_before_guard, before) = build_fixture("diff_noreport_a");
    let (_after_guard, after) = build_fixture("diff_noreport_b");
    let out = run_verify_semantic_diff(&json!({
        "before_graph_path": before.to_str().unwrap(),
        "after_graph_path": after.to_str().unwrap(),
    }));
    assert_eq!(out["status"], "ok", "response: {out}");
    assert!(out["report_path"].is_null(), "{out}");
}

// ---------------------------------------------------------------------------
// validate_prd_against_graph
// ---------------------------------------------------------------------------

#[test]
fn validate_prd_validates_its_arguments() {
    assert_eq!(
        do_validate_prd_against_graph(&json!("x")).expect_err("non-object"),
        "arguments must be an object"
    );
    assert_eq!(
        do_validate_prd_against_graph(&json!({})).expect_err("no prd_path"),
        "missing required field 'prd_path'"
    );
    assert_eq!(
        do_validate_prd_against_graph(&json!({"prd_path": "/a/prd.md"}))
            .expect_err("no graph_path"),
        "missing required field 'graph_path'"
    );
    let err = do_validate_prd_against_graph(&json!({"prd_path": "rel.md", "graph_path": "/g"}))
        .expect_err("relative prd_path");
    assert!(
        err.starts_with("prd_path must be an absolute path"),
        "{err}"
    );
}

#[test]
fn run_validate_prd_wraps_failures_in_the_stage_6_envelope() {
    let out = run_validate_prd_against_graph(&json!(null));
    assert_eq!(out["stage"], 6);
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "validate_prd_against_graph_failed");
}

#[test]
fn validate_prd_checks_a_prd_against_a_real_graph() {
    let (_graph_guard, graph) = build_fixture("validate");
    let dir = tmp_dir("validate_prd");
    let prd = dir.join("prd.md");
    fs::write(
        &prd,
        "# PRD\n\nChange `svc.rs::run_a` and `svc.rs::not_a_real_symbol`.\n",
    )
    .expect("write prd");

    let out = run_validate_prd_against_graph(&json!({
        "prd_path": prd.to_str().unwrap(),
        "graph_path": graph.to_str().unwrap(),
    }));

    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["tool"], "validate_prd_against_graph");
    assert!(out["validation_status"].is_string());
    for field in [
        "claimed_symbols",
        "resolved_symbols",
        "hallucinated_symbols",
        "unverifiable_symbols",
        "communities_spanned",
        "processes_impacted",
    ] {
        assert!(out["summary"][field].is_number(), "{field} missing: {out}");
    }
    // No run_id/finding_id/output_dir triple, so nothing is persisted.
    assert!(out["artifact_path"].is_null(), "{out}");
}

// ---------------------------------------------------------------------------
// extract_optional_ids
// ---------------------------------------------------------------------------

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().expect("object").clone()
}

#[test]
fn optional_ids_are_all_none_when_absent() {
    let (r, f, o) = extract_optional_ids(&obj(json!({}))).expect("absent is legal");
    assert!(r.is_none() && f.is_none() && o.is_none());
}

#[test]
fn optional_ids_pass_through_when_valid() {
    let (r, f, o) = extract_optional_ids(&obj(json!({
        "run_id": "run-1", "finding_id": "F-2", "output_dir": "/tmp/out",
    })))
    .expect("valid ids");
    assert_eq!(r.as_deref(), Some("run-1"));
    assert_eq!(f.as_deref(), Some("F-2"));
    assert_eq!(o.as_deref(), Some(Path::new("/tmp/out")));
}

#[test]
fn optional_ids_are_validated_when_present() {
    // "Optional" governs presence, not safety: a supplied id is still a path
    // segment and is still checked.
    let err = extract_optional_ids(&obj(json!({"run_id": "../up"}))).expect_err("unsafe run_id");
    assert!(err.starts_with("unsafe run_id"), "{err}");

    let err =
        extract_optional_ids(&obj(json!({"finding_id": "a/b"}))).expect_err("unsafe finding_id");
    assert!(err.starts_with("unsafe finding_id"), "{err}");

    let err = extract_optional_ids(&obj(json!({"output_dir": "rel/out"})))
        .expect_err("relative output_dir");
    assert!(
        err.starts_with("output_dir must be an absolute path"),
        "{err}"
    );
}

#[test]
fn optional_ids_are_independent_of_each_other() {
    // A run_id without an output_dir is legal — the triple is only required
    // together at the write site, not at parse time.
    let (r, f, o) = extract_optional_ids(&obj(json!({"run_id": "run-1"}))).expect("partial");
    assert_eq!(r.as_deref(), Some("run-1"));
    assert!(f.is_none() && o.is_none());
}

// ---------------------------------------------------------------------------
// check_security_gates
// ---------------------------------------------------------------------------

#[test]
fn check_security_gates_validates_its_arguments() {
    assert_eq!(
        do_check_security_gates(&json!(null)).expect_err("non-object"),
        "arguments must be an object"
    );
    assert_eq!(
        do_check_security_gates(&json!({})).expect_err("no graph_path"),
        "missing required field 'graph_path'"
    );
    let err = do_check_security_gates(&json!({"graph_path": "/tmp/nope-xyzzy"}))
        .expect_err("absent graph");
    assert!(err.starts_with("graph_path does not exist:"), "{err}");
}

#[test]
fn check_security_gates_requires_the_changed_symbol_array() {
    // Reached only after the graph exists, so it needs a real one. An absent
    // array is not an empty change set — it is a malformed call.
    let (_graph_guard, graph) = build_fixture("gates_args");
    let err = do_check_security_gates(&json!({"graph_path": graph.to_str().unwrap()}))
        .expect_err("no changed_symbols");
    assert!(err.contains("changed_symbols"), "{err}");
}

#[test]
fn run_check_security_gates_wraps_failures_in_the_stage_8_envelope() {
    let out = run_check_security_gates(&json!(null));
    assert_eq!(out["stage"], 8);
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "check_security_gates_failed");
}

#[test]
fn check_security_gates_reports_a_verdict_and_counts() {
    let (_graph_guard, graph) = build_fixture("gates");
    let out = run_check_security_gates(&json!({
        "graph_path": graph.to_str().unwrap(),
        "changed_symbols": ["svc.rs::run_a"],
    }));

    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["tool"], "check_security_gates");
    assert!(out["gates_passed"].is_boolean(), "{out}");
    for field in [
        "changed_symbols",
        "critical_count",
        "warning_count",
        "info_count",
    ] {
        assert!(out["summary"][field].is_number(), "{field} missing: {out}");
    }
    assert!(
        out["artifact_path"].is_null(),
        "no id triple, no artifact: {out}"
    );
}

#[test]
fn non_string_entries_in_changed_symbols_are_dropped_not_fatal() {
    // `filter_map(as_str)` silently skips non-strings. That is the shipped
    // behaviour; pinning it means a later change to reject them is a
    // deliberate decision rather than an accident.
    let (_graph_guard, graph) = build_fixture("gates_mixed");
    let out = run_check_security_gates(&json!({
        "graph_path": graph.to_str().unwrap(),
        "changed_symbols": ["svc.rs::run_a", 7, null],
    }));
    assert_eq!(out["status"], "ok", "response: {out}");
    assert_eq!(out["summary"]["changed_symbols"], json!(1));
}

// ---------------------------------------------------------------------------
// The artifact-write decision — the same rule in both handlers
// ---------------------------------------------------------------------------

#[test]
fn security_artifacts_are_written_only_with_the_full_id_triple() {
    let (_graph_guard, graph) = build_fixture("gates_write");
    let out_dir = tmp_dir("gates_write_out");

    // Any incomplete combination writes nothing.
    for partial in [
        json!({"run_id": "r1"}),
        json!({"finding_id": "F1"}),
        json!({"output_dir": out_dir.to_str().unwrap()}),
        json!({"run_id": "r1", "finding_id": "F1"}),
        json!({"run_id": "r1", "output_dir": out_dir.to_str().unwrap()}),
    ] {
        let mut args = obj(json!({
            "graph_path": graph.to_str().unwrap(),
            "changed_symbols": ["svc.rs::run_a"],
        }));
        for (k, v) in partial.as_object().unwrap() {
            args.insert(k.clone(), v.clone());
        }
        let out = run_check_security_gates(&Value::Object(args));
        assert_eq!(out["status"], "ok", "response: {out}");
        assert!(
            out["artifact_path"].is_null(),
            "an incomplete triple must not write: {partial} -> {out}"
        );
    }

    // The complete triple writes, and the promised path exists.
    let out = run_check_security_gates(&json!({
        "graph_path": graph.to_str().unwrap(),
        "changed_symbols": ["svc.rs::run_a"],
        "run_id": "r1",
        "finding_id": "F1",
        "output_dir": out_dir.to_str().unwrap(),
    }));
    assert_eq!(out["status"], "ok", "response: {out}");
    let artifact = out["artifact_path"]
        .as_str()
        .expect("a complete triple must write an artifact");
    assert!(Path::new(artifact).exists(), "no file at {artifact}");
    // …and it lands under the run/finding layout every other stage reads.
    assert!(artifact.contains("runs"), "{artifact}");
    assert!(artifact.contains("r1"), "{artifact}");
    assert!(artifact.contains("F1"), "{artifact}");
}

#[test]
fn validation_artifacts_follow_the_same_triple_rule() {
    let (_graph_guard, graph) = build_fixture("validate_write");
    let out_dir = tmp_dir("validate_write_out");
    let prd_dir = tmp_dir("validate_write_prd");
    let prd = prd_dir.join("prd.md");
    fs::write(&prd, "# PRD\n\nTouch `svc.rs::run_a`.\n").expect("write prd");

    let without = run_validate_prd_against_graph(&json!({
        "prd_path": prd.to_str().unwrap(),
        "graph_path": graph.to_str().unwrap(),
        "run_id": "r1",
        "finding_id": "F1",
    }));
    assert_eq!(without["status"], "ok", "response: {without}");
    assert!(
        without["artifact_path"].is_null(),
        "no output_dir: {without}"
    );

    let with = run_validate_prd_against_graph(&json!({
        "prd_path": prd.to_str().unwrap(),
        "graph_path": graph.to_str().unwrap(),
        "run_id": "r1",
        "finding_id": "F1",
        "output_dir": out_dir.to_str().unwrap(),
    }));
    assert_eq!(with["status"], "ok", "response: {with}");
    let artifact = with["artifact_path"].as_str().expect("artifact written");
    assert!(Path::new(artifact).exists(), "no file at {artifact}");
}
