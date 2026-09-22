//! Issue #292: folds the LSP pass's `unlinked-file` verdicts into the coverage
//! report, and renders the cross-check. The coverage sidecar is written before
//! the LSP phase runs, so the merge happens here, in the composition root, not
//! inside the pass. source: ADR-9845.

use crate::indexer::coverage::{self, CoverageReport};
use crate::indexing_handlers::COVERAGE_LIST_CAP;
use crate::lsp_client::{UnlinkedFileCheck, UnlinkedFileFinding};
use serde_json::{json, Value};
use std::path::Path;

/// Records every unlinked finding in `report` (existing gaps win). Returns
/// whether anything was offered for merging.
pub(super) fn merge_unlinked(report: &mut CoverageReport, check: &UnlinkedFileCheck) -> bool {
    for finding in &check.unlinked {
        report.record_unlinked_file(&finding.rel_path, coverage_detail(finding));
    }
    !check.unlinked.is_empty()
}

/// `lsp_resolve`'s merge: the graph's sidecar lives in its `output_dir` (the
/// graph directory's parent). Never invents a report when none exists.
pub(super) fn merge_into_sidecar(graph_path: &Path, check: &UnlinkedFileCheck) -> Value {
    if check.unlinked.is_empty() {
        return json!("nothing_to_merge");
    }
    let Some(path) = graph_path.parent().map(coverage::coverage_path) else {
        return json!("unavailable");
    };
    let Some(mut report) = coverage::load(&path) else {
        return json!("unavailable");
    };
    merge_unlinked(&mut report, check);
    match coverage::save(&path, &report) {
        Ok(()) => json!("merged"),
        Err(e) => json!(format!("failed: {e}")),
    }
}

/// The `unlinked_file_check` block of `lsp_resolve` / `analyze_codebase`.
pub(super) fn unlinked_check_json(check: &UnlinkedFileCheck) -> Value {
    let unlinked: Vec<Value> = check
        .unlinked
        .iter()
        .take(COVERAGE_LIST_CAP)
        .map(|f| json!({"path": f.rel_path, "cargo_attribution": f.cargo_attribution.as_str()}))
        .collect();
    let contradicted: Vec<&String> = check
        .linked_despite_outside_targets
        .iter()
        .take(COVERAGE_LIST_CAP)
        .collect();
    json!({
        "pull_supported": check.pull_supported,
        "files_checked": check.files_checked,
        "unlinked_count": check.unlinked.len(),
        "unlinked_files": unlinked,
        "linked_despite_outside_build_targets_count": check.linked_despite_outside_targets.len(),
        "linked_despite_outside_build_targets": contradicted,
    })
}

/// The sidecar `detail`: rust-analyzer's first message line, and what cargo
/// said, so a disagreement stays visible in `query_graph(graph="missed")`.
fn coverage_detail(finding: &UnlinkedFileFinding) -> String {
    let message = finding.message.lines().next().unwrap_or_default();
    format!(
        "rust-analyzer unlinked-file: {message} (cargo metadata: {})",
        finding.cargo_attribution.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::coverage::{CoverageKind, FileCoverage};
    use crate::lsp_client::CargoAttribution;

    fn check() -> UnlinkedFileCheck {
        UnlinkedFileCheck {
            pull_supported: true,
            files_checked: 3,
            unlinked: vec![
                UnlinkedFileFinding {
                    rel_path: "kani/h.rs".into(),
                    message: "not included in any crates\n\nsilence it with ...".into(),
                    cargo_attribution: CargoAttribution::OutsideBuildTargets,
                },
                UnlinkedFileFinding {
                    rel_path: "src/orphan.rs".into(),
                    message: "not included anywhere in the module tree".into(),
                    cargo_attribution: CargoAttribution::InsideBuildTargets,
                },
            ],
            linked_despite_outside_targets: vec!["fuzz/f.rs".into()],
        }
    }

    #[test]
    fn a_disagreement_becomes_a_gap_and_an_agreement_keeps_the_cargo_gap() {
        let mut report = CoverageReport::new("full", 3);
        report.files.insert(
            "kani/h.rs".into(),
            FileCoverage {
                kind: CoverageKind::OutsideBuildTargets,
                detail: "cargo".into(),
                error_ranges: vec![],
            },
        );
        assert!(merge_unlinked(&mut report, &check()));
        assert_eq!(
            report.files["kani/h.rs"].kind,
            CoverageKind::OutsideBuildTargets
        );
        let orphan = &report.files["src/orphan.rs"];
        assert_eq!(orphan.kind, CoverageKind::UnlinkedFile);
        assert_eq!(
            orphan.detail,
            "rust-analyzer unlinked-file: not included anywhere in the module tree \
             (cargo metadata: inside_build_targets)"
        );
    }

    #[test]
    fn the_cross_check_renders_both_directions_of_disagreement() {
        let rendered = unlinked_check_json(&check());
        assert_eq!(rendered["unlinked_count"], 2);
        assert_eq!(rendered["unlinked_files"][1]["path"], "src/orphan.rs");
        assert_eq!(
            rendered["unlinked_files"][1]["cargo_attribution"],
            "inside_build_targets"
        );
        assert_eq!(
            rendered["linked_despite_outside_build_targets"],
            json!(["fuzz/f.rs"])
        );
    }

    #[test]
    fn lsp_resolve_never_invents_a_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph = dir.path().join("graph");
        assert_eq!(merge_into_sidecar(&graph, &check()), json!("unavailable"));
        assert!(!coverage::coverage_path(dir.path()).exists());
        coverage::save(
            &coverage::coverage_path(dir.path()),
            &CoverageReport::new("full", 1),
        )
        .expect("seed sidecar");
        assert_eq!(merge_into_sidecar(&graph, &check()), json!("merged"));
        let merged = coverage::load(&coverage::coverage_path(dir.path())).expect("load");
        assert_eq!(merged.counts().unlinked_file, 2);
    }
}
