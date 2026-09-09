// indexing_handlers_coverage — the coverage SURFACE: how an index's coverage
// record is rendered for a caller, and the `query_graph(graph="missed")` view.
//
// Split out of `indexing_handlers.rs` to keep it under the coding-standards
// section 4.1 cap, following the `tool_schemas_*.rs` sibling convention this
// repository already uses. This half reads a `CoverageReport` and shapes JSON;
// it never produces one.
// source: ADR-9841.

use std::path::Path;

use serde_json::{json, Value};

use crate::indexer;
use crate::indexing_handlers::{COVERAGE_CAVEAT, COVERAGE_LIST_CAP};

/// The capped example-file lists `coverage_summary` renders, one per kind —
/// split out of `coverage_summary` (Fowler "Extract Function", §4.2: adding
/// the `outside_build_targets` bucket, issue #284, pushed the single-function
/// version past the 50-line cap) so the JSON-shaping function stays a plain
/// assembly of already-computed pieces.
struct CoverageFileBuckets {
    partial_files: Vec<Value>,
    skipped_files: Vec<Value>,
    quarantined_files: Vec<Value>,
    outside_build_target_files: Vec<Value>,
    user_excluded_count: u64,
}

/// Walks `report.files` once, sorting each entry into its kind's
/// budget-capped example list (issue #249's `user_excluded_count` is tallied
/// in the same pass since it is itself a `Skipped`-kind sub-count).
fn bucket_coverage_files(report: &indexer::coverage::CoverageReport) -> CoverageFileBuckets {
    use indexer::coverage::CoverageKind;
    let mut b = CoverageFileBuckets {
        partial_files: Vec::new(),
        skipped_files: Vec::new(),
        quarantined_files: Vec::new(),
        outside_build_target_files: Vec::new(),
        user_excluded_count: 0,
    };
    for (rel, cov) in &report.files {
        match cov.kind {
            CoverageKind::ParsePartial => {
                if b.partial_files.len() < COVERAGE_LIST_CAP {
                    b.partial_files
                        .push(json!({"path": rel, "error_ranges": cov.error_ranges}));
                }
            }
            CoverageKind::Skipped => {
                if cov.detail == "user_excluded" {
                    b.user_excluded_count += 1;
                }
                if b.skipped_files.len() < COVERAGE_LIST_CAP {
                    b.skipped_files
                        .push(json!({"path": rel, "reason": cov.detail}));
                }
            }
            CoverageKind::Quarantined => {
                if b.quarantined_files.len() < COVERAGE_LIST_CAP {
                    b.quarantined_files
                        .push(json!({"path": rel, "reason": cov.detail}));
                }
            }
            CoverageKind::OutsideBuildTargets => {
                if b.outside_build_target_files.len() < COVERAGE_LIST_CAP {
                    b.outside_build_target_files.push(json!(rel));
                }
            }
        }
    }
    b
}

/// Renders a `CoverageReport` into an honest, budget-bounded JSON block: exact
/// counts per kind, capped example lists (parse_incomplete carries error ranges;
/// skipped/quarantined/outside_build_targets carry reasons), and the
/// completeness caveat.
/// A capped, deterministic sample of the pruned directories, newest-sorted by
/// path so the list is stable across runs. The full map lives in the sidecar.
fn pruned_dir_sample(report: &indexer::coverage::CoverageReport) -> Value {
    const MAX: usize = 25;
    let sample: Vec<Value> = report
        .pruned_dirs
        .iter()
        .take(MAX)
        .map(|(rel, reason)| json!({ "path": rel, "reason": reason }))
        .collect();
    json!(sample)
}

pub(crate) fn coverage_summary(report: &indexer::coverage::CoverageReport) -> Value {
    let counts = report.counts();
    let b = bucket_coverage_files(report);
    json!({
        "index_mode": report.index_mode,
        "files_indexed": report.files_indexed,
        "parse_incomplete": { "count": counts.parse_partial, "files": b.partial_files },
        "skipped": {
            "count": counts.skipped,
            "user_excluded_count": b.user_excluded_count,
            "files": b.skipped_files
        },
        "quarantined": { "count": counts.quarantined, "files": b.quarantined_files },
        "outside_build_targets": {
            "count": counts.outside_build_targets,
            "files": b.outside_build_target_files
        },
        // Declared policy, NOT a gap: reported so a reader can see what the
        // walk refused to enter, and deliberately outside the four gap buckets
        // so `.git`/`target` do not make every graph incomplete.
        // source: ADR-9841.
        "pruned_dirs": {
            "count": report.pruned_dirs.len(),
            "dirs": pruned_dir_sample(report),
        },
        "caveat": COVERAGE_CAVEAT,
    })
}

/// Loads the coverage sidecar for a graph at `graph_dir` (its `output_dir` is the
/// parent) and renders the summary, or `null` when no coverage is available.
pub(crate) fn coverage_summary_for_graph(graph_dir: &Path) -> Value {
    match graph_dir.parent() {
        Some(output_dir) => {
            let path = indexer::coverage::coverage_path(output_dir);
            match indexer::coverage::load(&path) {
                Some(report) => coverage_summary(&report),
                None => Value::Null,
            }
        }
        None => Value::Null,
    }
}

/// The `query_graph(graph="missed")` response (issue #57): a structural
/// enumeration of what the index does NOT fully cover, so an agent can pivot to
/// grep. `null` coverage means no coverage sidecar exists for this graph — which
/// is itself NOT a completeness claim (the index may predate coverage tracking).
pub(crate) fn query_missed_response(graph_path: &Path) -> Value {
    let coverage = coverage_summary_for_graph(graph_path);
    if coverage.is_null() {
        return json!({
            "stage": 3,
            "status": "ok",
            "tool": "query_graph",
            "graph": "missed",
            "coverage": Value::Null,
            "note": "No coverage sidecar for this graph (indexed before coverage \
                     tracking, or the sidecar was not carried alongside the graph). \
                     Absence of coverage data is NOT a completeness guarantee — \
                     re-index to generate it.",
        });
    }
    json!({
        "stage": 3,
        "status": "ok",
        "tool": "query_graph",
        "graph": "missed",
        "coverage": coverage,
        "note": "These files/ranges are where the index is known to be incomplete \
                 — prefer grep for them before trusting a negative graph result.",
    })
}
