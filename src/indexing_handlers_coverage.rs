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
    unlinked_files: Vec<Value>,
    feature_gated_files: Vec<Value>,
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
        unlinked_files: Vec::new(),
        feature_gated_files: Vec::new(),
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
            CoverageKind::UnlinkedFile => {
                if b.unlinked_files.len() < COVERAGE_LIST_CAP {
                    b.unlinked_files.push(json!(rel));
                }
            }
            CoverageKind::FeatureGated => {
                if b.feature_gated_files.len() < COVERAGE_LIST_CAP {
                    b.feature_gated_files
                        .push(json!({"path": rel, "reason": cov.detail}));
                }
            }
        }
    }
    b
}

/// Renders a `CoverageReport` into an honest, budget-bounded JSON block: exact
/// counts per kind, capped example lists (parse_incomplete carries error ranges;
/// skipped/quarantined/feature_gated carry reasons), and the
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

/// The `cargo_attribution` block (issue #316): whether the two Cargo-derived
/// buckets (`outside_build_targets`, `feature_gated`) carry information.
/// `not_recorded` is a sidecar written before the field existed; it is
/// deliberately distinct from `unknown`, which this binary emits only when it
/// tried and failed to build the target map.
fn cargo_attribution_json(report: &indexer::coverage::CoverageReport) -> Value {
    match &report.cargo_attribution {
        Some(status) => json!({ "status": status.as_str(), "detail": status.detail() }),
        None => json!({
            "status": "not_recorded",
            "detail": "this coverage record predates cargo-attribution tracking \
                       (issue #316); re-index to record it",
        }),
    }
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
        "unlinked_file": { "count": counts.unlinked_file, "files": b.unlinked_files },
        "feature_gated": {
            "count": counts.feature_gated,
            "files": b.feature_gated_files
        },
        "cargo_attribution": cargo_attribution_json(report),
        // Declared policy, NOT a gap: reported so a reader can see what the
        // walk refused to enter, and deliberately outside the gap buckets
        // so `.git`/`target` do not make every graph incomplete.
        // source: ADR-9841.
        "pruned_dirs": {
            "count": report.pruned_dirs.len(),
            "dirs": pruned_dir_sample(report),
        },
        "caveat": COVERAGE_CAVEAT,
    })
}

/// Issue #353: what `index_status` says about the `#[cfg]` twins of a graph.
///
/// `cfg_twins` counts the twins and their state under the DEFAULT build profile
/// (default features; `cfg(kani)`, `test` and target options stay `unknown`) and
/// the call sites left open because a twin set is not decided. `feature_gated`
/// covers modules through its `files` and, through `items`, the twin ITEMS the
/// default build compiles out: the same word, one level down, so an item under
/// `#[cfg(feature = "x")]` is not invisible just because its file is compiled.
/// `null` when the graph cannot be opened.
pub(crate) fn cfg_twin_status(graph_dir: &Path) -> Option<(Value, Value)> {
    let store = crate::graph_store::GraphStore::open_or_create(graph_dir).ok()?;
    let s = store.cfg_twin_summary();
    let examples: Vec<Value> = s
        .inactive_examples
        .iter()
        .map(|t| json!({ "id": t.id, "cfg_gate": t.cfg_gate }))
        .collect();
    let twins = json!({
        "sets": s.sets,
        "members": s.members,
        "active": s.active,
        "inactive": s.inactive,
        "unknown": s.unknown,
        "unresolved_sites": s.unresolved_sites,
        "profile": "default features; options such as kani, test and unix are unknown",
    });
    let items = json!({ "count": s.inactive, "examples": examples });
    Some((twins, items))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::cargo_attribution::CargoAttributionStatus;
    use crate::indexer::coverage::CoverageReport;

    fn rendered(status: Option<CargoAttributionStatus>) -> Value {
        let mut report = CoverageReport::new("full", 1);
        report.cargo_attribution = status;
        coverage_summary(&report)["cargo_attribution"].clone()
    }

    #[test]
    fn a_known_map_renders_known_with_no_detail() {
        assert_eq!(
            rendered(Some(CargoAttributionStatus::Known)),
            json!({"status": "known", "detail": null})
        );
    }

    #[test]
    fn an_unknown_map_renders_unknown_with_its_cause() {
        let detail = "cargo metadata failed (exit status: 101): error: current package \
                      believes it's in a workspace when it's not";
        assert_eq!(
            rendered(Some(CargoAttributionStatus::Unknown {
                detail: detail.into()
            })),
            json!({"status": "unknown", "detail": detail})
        );
    }

    #[test]
    fn a_corpus_without_cargo_renders_not_applicable() {
        assert_eq!(
            rendered(Some(CargoAttributionStatus::NotApplicable {
                detail: "no Cargo.toml at the analyzed root".into()
            })),
            json!({"status": "not_applicable", "detail": "no Cargo.toml at the analyzed root"})
        );
    }

    /// An old sidecar never claims `unknown`: it was not analysed for this.
    #[test]
    fn an_old_sidecar_renders_not_recorded_never_unknown() {
        assert_eq!(rendered(None)["status"], json!("not_recorded"));
    }
}
