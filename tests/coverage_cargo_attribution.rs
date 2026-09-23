// coverage_cargo_attribution — issue #316 gate (library level, no language
// server).
//
// When `cargo metadata` fails (the #282 shape: a crate nested under a parent
// `[workspace] members = []`), no file can be attributed outside the build or
// feature-gated, and both buckets come back empty — which looked exactly like
// a clean Rust corpus. The report must now say the target map is unknown, and
// why; a corpus with no `Cargo.toml` must say not_applicable instead.
//
// The status is read through the report's serialized JSON, not the Rust
// field, so this file compiles against a tree that predates the field and
// fails there on the assertion: the before/after gate.

use ai_architect_mcp::indexer::{self, coverage, manifest, IndexOptions};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

const CRATE_MANIFEST: &str =
    "[package]\nname = \"nested_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n";

fn cargo_available() -> bool {
    Command::new("cargo").arg("--version").output().is_ok()
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, body).expect("write");
}

fn full_index(repo: &Path, out: &Path) -> coverage::CoverageReport {
    fs::create_dir_all(out).expect("mk out");
    let result =
        indexer::index_codebase_with_language(repo, &out.join("graph"), &IndexOptions::default())
            .expect("index");
    coverage::save(&coverage::coverage_path(out), &result.coverage).expect("save coverage");
    result.coverage
}

fn attribution(report: &coverage::CoverageReport) -> Value {
    serde_json::to_value(report).expect("report serializes")["cargo_attribution"].clone()
}

#[test]
fn a_nested_crate_cargo_refuses_reports_an_unknown_target_map_then_known_once_fixed() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH (the failing cargo metadata is the fixture)");
        return;
    }
    let tmp = tempfile::tempdir().expect("temp dir");
    write(tmp.path(), "Cargo.toml", "[workspace]\nmembers = []\n");
    let repo = tmp.path().join("child");
    write(&repo, "Cargo.toml", CRATE_MANIFEST);
    write(&repo, "src/lib.rs", "pub fn f() -> u8 { 1 }\n");
    write(&repo, "kani/h.rs", "fn proof() { let _ = f(); }\n");
    let out = tmp.path().join("out");

    let report = full_index(&repo, &out);
    let status = attribution(&report);
    assert_eq!(status["status"], "unknown", "{status}");
    let detail = status["detail"].as_str().expect("detail");
    assert!(detail.contains("believes it's in a workspace"), "{detail}");
    assert!(
        report.files.is_empty(),
        "nothing attributed: {:?}",
        report.files
    );

    // Only Cargo.toml changes: the incremental pass must recompute the
    // status, not carry `unknown` forward, and attribute the harness.
    let manifest_path = manifest::manifest_path(&out);
    indexer::write_full_manifest(&repo, &manifest_path, &IndexOptions::default())
        .expect("manifest");
    write(
        &repo,
        "Cargo.toml",
        &format!("{CRATE_MANIFEST}\n[workspace]\n"),
    );
    let prior = manifest::load(&manifest_path).expect("load manifest");
    indexer::index_incremental(
        &repo,
        &out.join("graph"),
        &manifest_path,
        &IndexOptions::default(),
        &prior,
    )
    .expect("incremental");
    let cov = coverage::load(&coverage::coverage_path(&out)).expect("coverage sidecar");
    let status = attribution(&cov);
    assert_eq!(status["status"], "known", "{status}");
    assert!(cov.files.contains_key("kani/h.rs"), "{:?}", cov.files);
}

#[test]
fn a_corpus_without_cargo_toml_is_not_applicable_never_unknown() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("repo");
    write(
        &repo,
        "m.py",
        "def f():\n    return g()\n\ndef g():\n    return 1\n",
    );
    let report = full_index(&repo, &tmp.path().join("out"));
    assert_eq!(attribution(&report)["status"], "not_applicable");
}
