// coverage — the incremental coverage-sidecar merge path (issue #288 split).
//
// Carries forward unchanged files' coverage gaps, overlays the freshly
// reparsed files' fresh gaps, and recomputes the outside-build-targets gap
// class fresh every pass (issue #284). Separable from the change-detection
// path: this module only ever reads a `ChangeSet`/`Discovered` slice, never
// mutates the graph.

use std::collections::HashSet;
use std::path::Path;

use super::super::cargo_targets;
use super::super::coverage::{self, FileCoverage};
use super::{ChangeSet, Discovered};
use std::collections::BTreeMap;

/// Builds the next coverage report by carrying forward `prior` gaps for files
/// that were NOT reparsed (and still exist), overlaid with the fresh gaps for
/// the reparsed files. A reparsed file that is now clean simply has no fresh
/// entry, so its stale gap is DROPPED — this is how an incomplete/quarantined
/// file clears its flag once it becomes parseable (issue #57 item 5). Deleted
/// and renamed-old files are absent from `current_rels`, so their gaps drop too.
///
/// `OutsideBuildTargets` entries (issue #284) are DELIBERATELY EXCLUDED from
/// the carry-forward: the compiled-target set can flip with zero `.rs` files
/// touched (a `[[test]]` added to `Cargo.toml`), so carrying a prior file's
/// verdict forward would let it go stale silently. The caller
/// (`save_incremental_coverage`) recomputes them fresh every pass via
/// `overlay_outside_targets`.
fn merge_coverage(
    prior: Option<&coverage::CoverageReport>,
    reparsed_gaps: BTreeMap<String, FileCoverage>,
    reparsed_rels: &HashSet<String>,
    current_rels: &HashSet<String>,
    index_mode: &str,
    files_indexed: u64,
) -> coverage::CoverageReport {
    let mut report = coverage::CoverageReport::new(index_mode, files_indexed);
    // Carry forward prior gaps for files that still exist and were not
    // reparsed — except OutsideBuildTargets, recomputed every pass (see doc).
    if let Some(prior) = prior {
        for (rel, cov) in &prior.files {
            if current_rels.contains(rel)
                && !reparsed_rels.contains(rel)
                && cov.kind != coverage::CoverageKind::OutsideBuildTargets
            {
                report.files.insert(rel.clone(), cov.clone());
            }
        }
    }
    // Overlay fresh gaps for the reparsed files (clean reparsed files add nothing).
    for (rel, cov) in reparsed_gaps {
        report.files.insert(rel, cov);
    }
    // files_indexed = files with a covered surface = total minus not-indexed
    // (skipped/quarantined). Parse-partial and outside-build-targets files
    // WERE indexed (declarations are in the graph), so they count.
    let counts = report.counts();
    report.files_indexed = files_indexed.saturating_sub(counts.skipped + counts.quarantined);
    report
}

/// Recomputes `OutsideBuildTargets` coverage gaps (issue #284) for the CURRENT
/// tree in one pass, over every `.rs` file discovered — not just the reparsed
/// ones — because the compiled-target set can change without any file being
/// reparsed (see `merge_coverage`'s doc). Only inserts a gap where `report`
/// does not already carry one for that file: `entry().or_insert()`, so a
/// stronger existing gap (parse_partial/skipped/quarantined, carried forward
/// or freshly reparsed) is never downgraded — mirrors
/// `CoverageCollector::record_outside_targets`'s same rule on the full-index
/// path. Costs one `cargo metadata` subprocess per incremental pass, gated
/// (like the full-index path) on "a root `Cargo.toml` exists and at least one
/// `.rs` file is in the current tree".
fn overlay_outside_targets(
    report: &mut coverage::CoverageReport,
    codebase: &Path,
    current: &[Discovered],
) {
    let has_rust_file = current.iter().any(|d| d.rel.ends_with(".rs"));
    if !has_rust_file || !codebase.join("Cargo.toml").is_file() {
        return;
    }
    let map = cargo_targets::discover(codebase);
    if matches!(map, cargo_targets::TargetMap::Unknown) {
        return;
    }
    for d in current {
        if !d.rel.ends_with(".rs") {
            continue;
        }
        if map.is_outside_targets(Path::new(&d.rel)) {
            report
                .files
                .entry(d.rel.clone())
                .or_insert_with(|| FileCoverage {
                    kind: coverage::CoverageKind::OutsideBuildTargets,
                    detail: cargo_targets::OUTSIDE_TARGETS_DETAIL.to_string(),
                    error_ranges: Vec::new(),
                });
        }
    }
}

/// Builds and writes the coverage sidecar for an incremental pass or bootstrap
/// fill: carry forward unchanged files' gaps, overlay the reparsed files'
/// fresh gaps, drop gaps for vanished/now-clean files, then recompute
/// outside-build-targets fresh over the whole current tree (issue #284).
/// Best-effort — a failed write only degrades the honesty signal, never the
/// index.
pub(super) fn save_incremental_coverage(
    codebase: &Path,
    graph_dir: &Path,
    current: &[Discovered],
    changes: &ChangeSet,
    reparsed_gaps: BTreeMap<String, FileCoverage>,
    index_mode: &str,
) {
    let output_dir = match graph_dir.parent() {
        Some(p) => p,
        None => return,
    };
    let cov_path = coverage::coverage_path(output_dir);
    let prior = coverage::load(&cov_path);
    let current_rels: HashSet<String> = current.iter().map(|d| d.rel.clone()).collect();
    let mut reparsed_rels: HashSet<String> = HashSet::new();
    for d in &changes.changed {
        reparsed_rels.insert(d.rel.clone());
    }
    for d in &changes.added {
        reparsed_rels.insert(d.rel.clone());
    }
    for r in &changes.renamed {
        reparsed_rels.insert(r.new_file.rel.clone());
    }
    let mut report = merge_coverage(
        prior.as_ref(),
        reparsed_gaps,
        &reparsed_rels,
        &current_rels,
        index_mode,
        current.len() as u64,
    );
    overlay_outside_targets(&mut report, codebase, current);
    if let Err(e) = coverage::save(&cov_path, &report) {
        eprintln!("[ap] coverage sidecar write failed: {e}");
    }
}
