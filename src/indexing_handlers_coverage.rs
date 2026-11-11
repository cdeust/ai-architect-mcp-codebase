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

/// The `no_extraction` block (issue #447): files no symbol was extracted
/// from (no language parser handles their extension and the IaC pass drew no
/// node from them), as an exact total plus a per-extension count. The
/// `COVERAGE_LIST_CAP` largest extensions are kept (ties by name) and
/// `extensions_omitted` says how many were left out, so a capped list never
/// reads as the whole set. A sidecar written before the tally existed, or by
/// a `language`-filtered index, renders `not_recorded`, never a count of zero.
fn no_extraction_json(report: &indexer::coverage::CoverageReport) -> Value {
    let Some(tally) = &report.no_extraction else {
        return json!({
            "status": "not_recorded",
            "detail": "no tally recorded: this coverage record predates it \
                       (issue #447), or the index ran with a `language` filter, \
                       which walks only the files that language parses; re-index \
                       without a filter to record it",
        });
    };
    let mut by_count: Vec<(&String, &u64)> = tally.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let total: u64 = tally.values().sum();
    let listed: serde_json::Map<String, Value> = by_count
        .iter()
        .take(COVERAGE_LIST_CAP)
        .map(|(ext, n)| ((*ext).clone(), json!(n)))
        .collect();
    json!({
        "status": "recorded",
        "count": total,
        "by_extension": listed,
        "extensions_omitted": by_count.len().saturating_sub(COVERAGE_LIST_CAP),
        "note": "No symbols extracted: no language parser handles these \
                 extensions and the IaC pass drew no node from them, so what they \
                 declare is not in the graph and a search that returns nothing \
                 there is not evidence of absence — grep them. Markdown and shell \
                 files can still carry file-to-file reference edges. Not counted \
                 as a gap: data and doc files land here too.",
    })
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
        // Declared limit of the parsers, NOT a gap (issue #447): outside the
        // gap buckets for the reason `pruned_dirs` is.
        "no_extraction": no_extraction_json(report),
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
/// (default features; `kani`, `miri`, `doc` and `doctest` are false, `test` and target options stay `unknown`) and
/// the call sites left open because a twin set is not decided. `feature_gated`
/// covers modules through its `files` and, through `items`, the twin ITEMS the
/// default build compiles out: the same word, one level down, so an item under
/// `#[cfg(feature = "x")]` is not invisible just because its file is compiled.
/// Reads through the store the caller already opened (issue #379).
pub(crate) fn cfg_twin_status(store: &crate::graph_store::GraphStore) -> Option<(Value, Value)> {
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
        "profile": "default features; kani, miri, doc and doctest are false; options such as test and unix are unknown",
    });
    let items = json!({ "count": s.inactive, "examples": examples });
    Some((twins, items))
}

/// Every open call site by its reason (issue #393): the total, each reason's
/// count, how many no static resolver should resolve (`by_construction`) and how
/// many name a resolver gap (`improvable`), and whether the graph recorded the
/// reasons at all (`reasons_recorded`; an older graph reads `not_recorded`).
pub(crate) fn unresolved_call_sites(store: &crate::graph_store::GraphStore) -> Value {
    use crate::graph_store::callsite_reasons::{BY_CONSTRUCTION, IMPROVABLE};
    match store.unresolved_site_summary() {
        Ok(s) => json!({
            "total": s.total,
            "by_reason": s.by_reason,
            "by_construction": s.sum_of(&BY_CONSTRUCTION),
            "improvable": s.sum_of(&IMPROVABLE),
            "reasons_recorded": s.reasons_recorded,
        }),
        Err(e) => json!({ "error": e }),
    }
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

/// The `no_extraction` block `search_codebase` attaches to an answer with
/// zero results (issue #447): a symbol that lives in a PHP, C# or Lua file is
/// not in the graph, so "ok, 0 results" alone reads as absence. The block is
/// the coverage record's tally under the same keys, with a `note` worded for
/// the empty search when at least one file had no extraction. Without a
/// tally (no coverage sidecar, an older one, a `language`-filtered index) it
/// renders `status: not_recorded`, never a count of zero.
pub(crate) fn zero_result_no_extraction(graph_dir: &Path) -> Value {
    let report = graph_dir
        .parent()
        .and_then(|out| indexer::coverage::load(&indexer::coverage::coverage_path(out)));
    let Some(report) = report else {
        return json!({
            "status": "not_recorded",
            "detail": "no coverage record next to this graph, so the number of \
                       files no symbol was extracted from is unknown; zero \
                       results is not evidence of absence. Re-index to record it",
        });
    };
    let mut block = no_extraction_json(&report);
    match block["count"].as_u64() {
        // Nothing to grep: the coverage note would point at no file.
        Some(0) => {
            if let Some(fields) = block.as_object_mut() {
                fields.remove("note");
            }
        }
        Some(count) => {
            let files = if count == 1 { "file" } else { "files" };
            block["note"] = json!(format!(
                "no extraction for {count} {files} in this repository: no \
                 language parser handles their extension (`by_extension`) and \
                 the IaC pass drew no node from them, so a symbol they declare \
                 is not in the graph and this empty result is not evidence \
                 that it is absent; grep those files."
            ));
        }
        // `not_recorded`: the block already says why there is no count.
        None => {}
    }
    block
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
    use std::path::PathBuf;

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

    /// Issue #447: the tally is rendered with its exact total, the capped
    /// list keeps the largest extensions, and it says how many it left out.
    #[test]
    fn no_extraction_renders_total_keeps_largest_and_counts_the_omitted() {
        let mut report = CoverageReport::new("full", 1);
        let mut tally: std::collections::BTreeMap<String, u64> =
            [("lua".to_string(), 1), ("php".to_string(), 3)]
                .into_iter()
                .collect();
        for i in 0..COVERAGE_LIST_CAP {
            tally.insert(format!("x{i:02}"), 1);
        }
        report.no_extraction = Some(tally);
        let block = coverage_summary(&report)["no_extraction"].clone();
        assert_eq!(block["count"], json!(4 + COVERAGE_LIST_CAP as u64));
        let listed = block["by_extension"].as_object().expect("an object");
        assert_eq!(listed.len(), COVERAGE_LIST_CAP);
        assert_eq!(listed["php"], json!(3));
        assert_eq!(block["extensions_omitted"], json!(2));
        // Ties at count 1 are broken by name: `lua` sorts before `x00..`, so
        // it is kept and the last two `x` keys are the ones left out.
        assert!(listed.contains_key("lua"));
        assert!(!listed.contains_key(&format!("x{:02}", COVERAGE_LIST_CAP - 1)));
        assert!(!listed.contains_key(&format!("x{:02}", COVERAGE_LIST_CAP - 2)));
        assert_eq!(block["status"], json!("recorded"));
    }

    /// Issue #447: an old sidecar has no tally; it never renders as zero.
    #[test]
    fn an_old_sidecar_renders_no_extraction_not_recorded_never_zero() {
        let block = coverage_summary(&CoverageReport::new("full", 1))["no_extraction"].clone();
        assert_eq!(block["status"], json!("not_recorded"));
        assert!(block.get("count").is_none(), "{block}");
    }

    /// Writes `report` as the coverage sidecar of a graph directory and
    /// returns the temp dir (kept alive by the caller) with the graph path.
    fn graph_with_sidecar(report: Option<&CoverageReport>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        if let Some(report) = report {
            let path = indexer::coverage::coverage_path(dir.path());
            indexer::coverage::save(&path, report).expect("save sidecar");
        }
        let graph = dir.path().join("graph");
        (dir, graph)
    }

    /// Issue #447: an empty search names how many files had no extraction,
    /// in the singular for one file, and keeps the per-extension counts.
    #[test]
    fn a_zero_result_block_names_the_files_without_extraction() {
        let mut report = CoverageReport::new("full", 4);
        report.no_extraction = Some(
            [("lua".to_string(), 1), ("php".to_string(), 2)]
                .into_iter()
                .collect(),
        );
        let (_dir, graph) = graph_with_sidecar(Some(&report));
        let block = zero_result_no_extraction(&graph);
        assert_eq!(block["status"], json!("recorded"));
        assert_eq!(block["count"], json!(3));
        assert_eq!(block["by_extension"], json!({"lua": 1, "php": 2}));
        let note = block["note"].as_str().expect("a note");
        assert!(
            note.starts_with("no extraction for 3 files in this repository"),
            "{note}"
        );

        report.no_extraction = Some([("php".to_string(), 1)].into_iter().collect());
        let (_dir, graph) = graph_with_sidecar(Some(&report));
        let note = zero_result_no_extraction(&graph)["note"].clone();
        assert!(
            note.as_str()
                .is_some_and(|n| n.starts_with("no extraction for 1 file in this repository")),
            "{note}"
        );
    }

    /// Issue #447: a recorded tally of zero says so and carries no note (no
    /// file to grep); a sidecar without a tally and a graph without a sidecar
    /// both render `not_recorded` with the reason, never a count.
    #[test]
    fn a_zero_result_block_never_turns_a_missing_tally_into_zero() {
        let mut report = CoverageReport::new("full", 1);
        report.no_extraction = Some(Default::default());
        let (_dir, graph) = graph_with_sidecar(Some(&report));
        let block = zero_result_no_extraction(&graph);
        assert_eq!(block["count"], json!(0));
        assert!(block.get("note").is_none(), "{block}");

        let (_dir, graph) = graph_with_sidecar(Some(&CoverageReport::new("full", 1)));
        let untallied = zero_result_no_extraction(&graph);
        assert_eq!(untallied["status"], json!("not_recorded"));
        assert!(untallied.get("count").is_none(), "{untallied}");
        assert!(untallied["detail"]
            .as_str()
            .is_some_and(|d| d.contains("language")));

        let (_dir, graph) = graph_with_sidecar(None);
        let absent = zero_result_no_extraction(&graph);
        assert_eq!(absent["status"], json!("not_recorded"));
        assert!(absent.get("count").is_none(), "{absent}");
        assert!(absent["detail"]
            .as_str()
            .is_some_and(|d| d.contains("no coverage record")));
    }

    /// An old sidecar never claims `unknown`: it was not analysed for this.
    #[test]
    fn an_old_sidecar_renders_not_recorded_never_unknown() {
        assert_eq!(rendered(None)["status"], json!("not_recorded"));
    }
}
