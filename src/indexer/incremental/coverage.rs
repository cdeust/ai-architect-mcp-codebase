// coverage — the incremental coverage-sidecar merge path (issue #288 split).
//
// Carries forward unchanged files' coverage gaps, overlays the freshly
// reparsed files' fresh gaps, and recomputes the outside-build-targets gap
// class (issue #284) and the feature-gated one (issue #291) fresh every pass. Separable from the change-detection
// path: this module only ever reads a `ChangeSet`/`Discovered` slice, never
// mutates the graph.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use super::super::cargo_attribution;
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
/// `OutsideBuildTargets` entries (issue #284) and `FeatureGated` entries
/// (issue #291, a `[features] default` edit flips them) are DELIBERATELY EXCLUDED from
/// the carry-forward: the compiled-target set can flip with zero `.rs` files
/// touched (a `[[test]]` added to `Cargo.toml`), so carrying a prior file's
/// verdict forward would let it go stale silently. The caller
/// (`save_incremental_coverage`) recomputes them fresh every pass via
/// `overlay_cargo_attributions`. `UnlinkedFile` entries (issue #292) are
/// excluded for the same reason — a `mod` added in another file links an
/// untouched one — but only the next LSP pass can recompute them (ADR-9845).
fn merge_coverage(
    carry: &CarryForward<'_>,
    reparsed_gaps: BTreeMap<String, FileCoverage>,
    index_mode: &str,
    files_indexed: u64,
) -> coverage::CoverageReport {
    let CarryForward {
        prior,
        reparsed_rels,
        current_rels,
    } = *carry;
    let mut report = coverage::CoverageReport::new(index_mode, files_indexed);
    // Carry forward prior gaps for files that still exist and were not
    // reparsed — except the cargo-derived kinds and UnlinkedFile, all
    // recomputed every pass (see doc).
    if let Some(prior) = prior {
        for (rel, cov) in &prior.files {
            if current_rels.contains(rel)
                && !reparsed_rels.contains(rel)
                && !matches!(
                    cov.kind,
                    coverage::CoverageKind::OutsideBuildTargets
                        | coverage::CoverageKind::UnlinkedFile
                        | coverage::CoverageKind::FeatureGated
                )
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

/// What `merge_coverage` carries forward from: the prior report and the two
/// file sets that decide which of its entries survive (§4.4 parameter object).
#[derive(Clone, Copy)]
struct CarryForward<'a> {
    prior: Option<&'a coverage::CoverageReport>,
    reparsed_rels: &'a HashSet<String>,
    current_rels: &'a HashSet<String>,
}

/// Recomputes `OutsideBuildTargets` coverage gaps (issue #284) for the CURRENT
/// tree in one pass, over every `.rs` file discovered — not just the reparsed
/// ones — because the compiled-target set can change without any file being
/// reparsed (see `merge_coverage`'s doc). Only inserts a gap where `report`
/// does not already carry one for that file: `entry().or_insert()`, so a
/// stronger existing gap (parse_partial/skipped/quarantined, carried forward
/// or freshly reparsed) is never downgraded — mirrors
/// `CoverageCollector::record_outside_targets`'s same rule on the full-index
/// path. The same map then recomputes the `FeatureGated` gaps (issue #291),
/// under the same `or_insert` rule, and the report's `cargo_attribution`
/// status (issue #316) is overwritten with this pass's, never carried
/// forward. `cargo_attribution::attribute` spawns `cargo metadata` only when
/// a root `Cargo.toml` exists and the tree holds a `.rs` file.
fn overlay_cargo_attributions(
    report: &mut coverage::CoverageReport,
    codebase: &Path,
    current: &[Discovered],
) -> cargo_attribution::CargoFacts {
    let rust_files: BTreeSet<PathBuf> = current
        .iter()
        .filter(|d| d.rel.ends_with(".rs"))
        .map(|d| PathBuf::from(&d.rel))
        .collect();
    let found = cargo_attribution::attribute(codebase, &rust_files);
    for rel in found.outside_targets {
        report.files.entry(rel).or_insert_with(|| FileCoverage {
            kind: coverage::CoverageKind::OutsideBuildTargets,
            detail: cargo_targets::OUTSIDE_TARGETS_DETAIL.to_string(),
            error_ranges: Vec::new(),
        });
    }
    for (rel, detail) in found.feature_gated {
        report.files.entry(rel).or_insert(FileCoverage {
            kind: coverage::CoverageKind::FeatureGated,
            detail,
            error_ranges: Vec::new(),
        });
    }
    let known = found.status == cargo_attribution::CargoAttributionStatus::Known;
    report.cargo_attribution = Some(found.status);
    cargo_attribution::CargoFacts {
        known,
        crate_names: found.crate_names,
        file_features: found.file_features,
        target_contexts: found.target_contexts,
        targets: found.targets,
        target_owners: found.target_owners,
    }
}

/// Applies what the Cargo map says to the graph, now that every node is
/// written. Best-effort: a failure degrades the graph to what it was before
/// the fact was known, which both readers treat as "not decided".
///
/// - Records the crate evidence of this pass (issues #348, #349 and #358): the
///   resolver accepts a receiver type named through a `use` only against the
///   facts recorded by the latest pass, so no earlier decision outlives them.
/// - Writes `cfg_active` on every `#[cfg]` twin (issue #353): a twin the
///   default build cannot be shown to compile stays `unknown`, never `active`.
/// - Writes `File.target_context` on every file (issue #354): what the Cargo
///   package says the file is; a file no target reaches gets `''`.
pub(in crate::indexer) fn apply_cargo_facts(
    store: &crate::graph_store::GraphStore,
    facts: &cargo_attribution::CargoFacts,
) {
    if let Err(e) = store.write_crate_evidence(&facts.crate_evidence()) {
        eprintln!("[ap] crate evidence not recorded: {e}");
    }
    if let Err(e) = super::super::cfg_active::write(store, &facts.file_features) {
        eprintln!("[ap] cfg_active pass skipped: {e}");
    }
    if let Err(e) = store.write_target_contexts(&facts.target_contexts) {
        eprintln!("[ap] target_context pass skipped: {e}");
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
    pruned_dirs: BTreeMap<String, String>,
    index_mode: &str,
) -> cargo_attribution::CargoFacts {
    let output_dir = match graph_dir.parent() {
        Some(p) => p,
        None => return cargo_attribution::CargoFacts::default(),
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
    let carry = CarryForward {
        prior: prior.as_ref(),
        reparsed_rels: &reparsed_rels,
        current_rels: &current_rels,
    };
    let mut report = merge_coverage(&carry, reparsed_gaps, index_mode, current.len() as u64);
    // The prunes this pass observed, so the incremental sidecar names what the
    // walk refused exactly as the full index does. Recomputed every pass rather
    // than carried forward: the pruned set changes the moment a directory is
    // added or removed. source: ADR-9841.
    report.pruned_dirs = pruned_dirs;
    let facts = overlay_cargo_attributions(&mut report, codebase, current);
    if let Err(e) = coverage::save(&cov_path, &report) {
        eprintln!("[ap] coverage sidecar write failed: {e}");
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gap(kind: coverage::CoverageKind) -> FileCoverage {
        FileCoverage {
            kind,
            detail: String::new(),
            error_ranges: Vec::new(),
        }
    }

    /// Issue #292 (ADR-9845): an `unlinked-file` verdict depends on OTHER files
    /// (a `mod` added elsewhere links an untouched one), and only an LSP pass
    /// can recompute it — so an incremental pass must drop it, not carry it.
    #[test]
    fn an_unlinked_file_verdict_is_not_carried_forward_but_a_parse_gap_is() {
        let mut prior = coverage::CoverageReport::new("full", 2);
        prior.files.insert(
            "src/orphan.rs".into(),
            gap(coverage::CoverageKind::UnlinkedFile),
        );
        prior.files.insert(
            "src/partial.rs".into(),
            gap(coverage::CoverageKind::ParsePartial),
        );
        let current: HashSet<String> = ["src/orphan.rs", "src/partial.rs"]
            .into_iter()
            .map(String::from)
            .collect();

        let carry = CarryForward {
            prior: Some(&prior),
            reparsed_rels: &HashSet::new(),
            current_rels: &current,
        };
        let merged = merge_coverage(&carry, BTreeMap::new(), "incremental", 2);

        assert!(
            !merged.files.contains_key("src/orphan.rs"),
            "a stale unlinked-file verdict must not survive an index pass"
        );
        assert_eq!(
            merged.files["src/partial.rs"].kind,
            coverage::CoverageKind::ParsePartial
        );
    }

    /// Issue #316: a prior sidecar's status is never carried forward; the
    /// incremental pass records its own, so a tree that stopped being a
    /// Cargo project stops claiming `known`.
    #[test]
    fn the_incremental_pass_records_its_own_cargo_attribution_status() {
        let mut prior = coverage::CoverageReport::new("full", 1);
        prior.cargo_attribution = Some(cargo_attribution::CargoAttributionStatus::Known);
        let current: HashSet<String> = ["m.py".to_string()].into_iter().collect();
        let carry = CarryForward {
            prior: Some(&prior),
            reparsed_rels: &HashSet::new(),
            current_rels: &current,
        };
        let mut merged = merge_coverage(&carry, BTreeMap::new(), "incremental", 1);
        let dir = tempfile::tempdir().expect("temp dir");
        overlay_cargo_attributions(&mut merged, dir.path(), &[]);
        let status = merged.cargo_attribution.expect("always set by this binary");
        assert_eq!(status.as_str(), "not_applicable");
    }
}
