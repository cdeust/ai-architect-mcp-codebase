// coverage_feature_gated — issue #291 gate (library level, no language server).
//
// A module declared behind `#[cfg(feature = "...")]` for a feature the default
// build does not enable sits INSIDE a compiled Cargo target's directory, so the
// #284 `outside_build_targets` check cannot see it, and the static resolver
// binds the calls inside it, so an LSP pass never opens it either (measured on
// this exact fixture, 2026-09-23: the PR #320 binary left `coverage.files`
// empty). Only the coverage sidecar can say it is compiled out.
//
// The kind is compared through its serde wire tag, not the enum variant, so
// this file compiles against a tree that predates the variant and fails there
// on the assertion — the before/after gate the issue asks for.

use ai_architect_mcp::indexer::{self, coverage, manifest, IndexOptions};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LIB_RS: &str = "pub struct TaskSet;\n\
impl TaskSet {\n    pub fn response_of(&self) -> u64 { 1 }\n}\n\
pub fn total(sets: &[TaskSet]) -> u64 { sets[0].response_of() }\n\n\
#[cfg(feature = \"extra\")]\npub mod extra;\n\n\
#[cfg(test)]\nmod tests;\n";
const EXTRA_RS: &str = "mod inner;\n\n\
pub fn extra_entry() -> u64 {\n    extra_helper() + 1\n}\n\n\
pub fn extra_helper() -> u64 {\n    extra_leaf() + inner::inner_leaf()\n}\n\n\
pub fn extra_leaf() -> u64 { 7 }\n";
const INNER_RS: &str = "pub fn inner_leaf() -> u64 { 2 }\n";
const TESTS_RS: &str = "#[test]\nfn t() { assert_eq!(super::total(&[super::TaskSet]), 1); }\n";

fn manifest_with_default(default: &str) -> String {
    format!(
        "[package]\nname = \"feature_gated_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [features]\ndefault = [{default}]\nextra = []\n"
    )
}

fn cargo_available() -> bool {
    Command::new("cargo").arg("--version").output().is_ok()
}

fn write_fixture(root: &Path, default: &str) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir_all(repo.join("src/extra")).expect("mk src/extra");
    fs::write(repo.join("Cargo.toml"), manifest_with_default(default)).expect("manifest");
    fs::write(repo.join("src/lib.rs"), LIB_RS).expect("lib.rs");
    fs::write(repo.join("src/extra.rs"), EXTRA_RS).expect("extra.rs");
    fs::write(repo.join("src/extra/inner.rs"), INNER_RS).expect("inner.rs");
    fs::write(repo.join("src/tests.rs"), TESTS_RS).expect("tests.rs");
    repo
}

fn full_index(repo: &Path, out: &Path) -> coverage::CoverageReport {
    fs::create_dir_all(out).expect("mk out");
    let result =
        indexer::index_codebase_with_language(repo, &out.join("graph"), &IndexOptions::default())
            .expect("index");
    coverage::save(&coverage::coverage_path(out), &result.coverage).expect("save coverage");
    result.coverage
}

fn kind_tag(report: &coverage::CoverageReport, rel: &str) -> Option<String> {
    report
        .files
        .get(rel)
        .map(|f| serde_json::to_value(f.kind).expect("kind serializes"))
        .and_then(|v| v.as_str().map(str::to_string))
}

#[test]
fn a_module_gated_behind_a_non_default_feature_is_flagged_with_its_submodules() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH (cargo metadata supplies the feature table)");
        return;
    }
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = write_fixture(tmp.path(), "");
    let out = tmp.path().join("out");
    let report = full_index(&repo, &out);

    for gated in ["src/extra.rs", "src/extra/inner.rs"] {
        assert_eq!(
            kind_tag(&report, gated).as_deref(),
            Some("feature_gated"),
            "{gated} is compiled out under default features and must say so: {:?}",
            report.files
        );
    }
    let detail = &report.files["src/extra.rs"].detail;
    assert!(detail.contains("feature = \"extra\""), "{detail}");
    for compiled in ["src/lib.rs", "src/tests.rs"] {
        assert_eq!(
            kind_tag(&report, compiled),
            None,
            "{compiled} is not feature-gated (cfg(test) is not a feature): {:?}",
            report.files
        );
    }
}

#[test]
fn a_module_whose_feature_is_on_by_default_is_not_flagged() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH (cargo metadata supplies the feature table)");
        return;
    }
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = write_fixture(tmp.path(), "\"extra\"");
    let report = full_index(&repo, &tmp.path().join("out"));
    assert!(
        report.files.is_empty(),
        "every module is compiled under default features: {:?}",
        report.files
    );
}

#[test]
fn turning_the_feature_on_by_default_clears_the_flag_on_incremental_reindex() {
    if !cargo_available() {
        eprintln!("skipping: cargo not on PATH (cargo metadata supplies the feature table)");
        return;
    }
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = write_fixture(tmp.path(), "");
    let out = tmp.path().join("out");
    let manifest_path = manifest::manifest_path(&out);
    let baseline = full_index(&repo, &out);
    indexer::write_full_manifest(&repo, &manifest_path, &IndexOptions::default())
        .expect("manifest");
    assert_eq!(
        kind_tag(&baseline, "src/extra.rs").as_deref(),
        Some("feature_gated")
    );

    // Only Cargo.toml changes: no `.rs` file is reparsed, so a carried-forward
    // flag would go stale. It must be recomputed.
    fs::write(repo.join("Cargo.toml"), manifest_with_default("\"extra\"")).expect("manifest");
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
    assert!(
        cov.files.is_empty(),
        "enabling `extra` by default must clear both flags: {:?}",
        cov.files
    );
}
