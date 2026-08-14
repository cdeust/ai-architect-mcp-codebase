// coverage_honesty — issue #57 gate (library level).
//
// The zetetic done-criterion: an intentionally CORRUPT fixture file must index
// the rest of the repo cleanly AND report itself as uncovered. Plus: skipped
// (oversized) files are recorded; and a parse-incomplete file that later becomes
// parseable clears its flag on the next incremental pass.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::coverage::CoverageKind;
use ai_architect_mcp::indexer::{self, coverage, manifest, IndexOptions};
use std::fs;
use std::path::Path;

/// A syntactically broken source file that tree-sitter recovers into ERROR/
/// MISSING nodes (so parse_errors > 0 → parse_partial), rather than a clean parse.
const BROKEN_PY: &str =
    "def broken(  \n    let x: i32 = ;\n    @#$%^& struct }{\nclass 123Invalid\n";
const GOOD_PY: &str = "def good_fn():\n    return 1\n";

fn full_index(repo: &Path, out: &Path) -> indexer::IndexResult {
    let graph = out.join("graph");
    let result = indexer::index_codebase_with_language(repo, &graph, &IndexOptions::default())
        .expect("index");
    // Persist the coverage sidecar the way the handler does, so sidecar-level
    // assertions (and the incremental carry-forward) have a baseline to read.
    coverage::save(&coverage::coverage_path(out), &result.coverage).expect("save coverage");
    result
}

#[test]
fn corrupt_file_is_reported_and_the_rest_indexes_cleanly() {
    let tmp = tempfile::Builder::new()
        .prefix("coverage_corrupt_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("src")).expect("mk src");
    fs::write(repo.join("src/good.py"), GOOD_PY).expect("write good");
    fs::write(repo.join("src/other.py"), "def other():\n    return 2\n").expect("write other");
    fs::write(repo.join("src/broken.py"), BROKEN_PY).expect("write broken");

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).expect("mk out");
    let result = full_index(&repo, &out);

    // The corrupt file is reported as parse_incomplete with concrete line ranges.
    let broken = result
        .coverage
        .files
        .get("src/broken.py")
        .expect("broken.py must be reported as uncovered");
    assert_eq!(broken.kind, CoverageKind::ParsePartial);
    assert!(
        !broken.error_ranges.is_empty(),
        "parse_incomplete must carry the ERROR line ranges, got {:?}",
        broken.error_ranges
    );

    // The clean files are NOT flagged (absence = no detected gap).
    assert!(!result.coverage.files.contains_key("src/good.py"));
    assert!(!result.coverage.files.contains_key("src/other.py"));

    // The rest of the repo indexed cleanly — the corrupt file did not abort it.
    let store = GraphStore::open_or_create(&out.join("graph")).expect("open graph");
    let q = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'good_fn' RETURN f.name")
        .expect("query");
    assert!(
        !q.rows.is_empty(),
        "good.py must be fully indexed despite the sibling corrupt file"
    );
    // The coverage sidecar was written and is loadable.
    let loaded = coverage::load(&coverage::coverage_path(&out)).expect("sidecar loads");
    assert_eq!(loaded.counts().0, 1, "exactly one parse_incomplete file");
}

#[test]
fn oversized_file_is_recorded_as_skipped() {
    let tmp = tempfile::Builder::new()
        .prefix("coverage_oversized_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).expect("mk repo");
    fs::write(repo.join("small.py"), GOOD_PY).expect("write small");
    // > 1 MB (MAX_PARSE_BYTES) of valid-but-huge content → skipped before parse.
    let huge = "# padding line to exceed the per-file parse cap\n".repeat(24_000);
    assert!(huge.len() as u64 > 1_048_576);
    fs::write(repo.join("huge.py"), &huge).expect("write huge");

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).expect("mk out");
    let result = full_index(&repo, &out);

    let cov = result
        .coverage
        .files
        .get("huge.py")
        .expect("oversized file must be reported");
    assert_eq!(cov.kind, CoverageKind::Skipped);
    assert!(
        cov.detail.contains("oversized"),
        "skip reason must name the cause, got {:?}",
        cov.detail
    );
    // The small file is still there.
    assert!(!result.coverage.files.contains_key("small.py"));
}

#[test]
fn incremental_clears_a_parse_incomplete_flag_once_fixed() {
    let tmp = tempfile::Builder::new()
        .prefix("coverage_clears_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("src")).expect("mk src");
    fs::write(repo.join("src/fixme.py"), BROKEN_PY).expect("write fixme");
    fs::write(repo.join("src/stillbad.py"), BROKEN_PY).expect("write stillbad");
    fs::write(repo.join("src/clean.py"), GOOD_PY).expect("write clean");

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).expect("mk out");
    let graph = out.join("graph");
    let manifest_path = manifest::manifest_path(&out);
    let result = full_index(&repo, &out);
    indexer::write_full_manifest(&repo, &manifest_path, &IndexOptions::default())
        .expect("manifest");
    // Baseline: both broken files flagged.
    assert!(result.coverage.files.contains_key("src/fixme.py"));
    assert!(result.coverage.files.contains_key("src/stillbad.py"));

    // Fix ONE of them; leave the other broken and untouched.
    fs::write(
        repo.join("src/fixme.py"),
        "def now_valid():\n    return 3\n",
    )
    .expect("fix");

    let prior = manifest::load(&manifest_path).expect("load manifest");
    indexer::index_incremental(
        &repo,
        &graph,
        &manifest_path,
        &IndexOptions::default(),
        &prior,
    )
    .expect("incremental");

    let cov = coverage::load(&coverage::coverage_path(&out)).expect("coverage sidecar");
    assert!(
        !cov.files.contains_key("src/fixme.py"),
        "the fixed file's parse_incomplete flag must clear on the incremental pass"
    );
    assert!(
        cov.files.contains_key("src/stillbad.py"),
        "an unchanged still-broken file's flag must carry forward"
    );
    // The fixed file's symbol is now in the graph.
    let store = GraphStore::open_or_create(&graph).expect("open graph");
    let q = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'now_valid' RETURN f.name")
        .expect("query");
    assert!(
        !q.rows.is_empty(),
        "the fixed file must now be fully indexed"
    );
}
