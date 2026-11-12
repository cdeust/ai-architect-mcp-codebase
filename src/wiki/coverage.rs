// wiki::coverage — `parse_incomplete` files attributed to the pages they
// affect (issue #196, criterion 5, the `parse_incomplete` half).
//
// The coverage record of an index (`CoverageReport`, the
// `index_coverage.json` sidecar) lists the files tree-sitter read with
// ERROR/MISSING ranges. Constructs in those files may be missing from the
// graph, so every page that lists a member defined in one of them carries a
// `CoverageGap::ParseIncomplete { file }`: the gap is visible on the page
// whose content it may truncate, instead of absent from it.
//
// Attribution is by file, the unit the record and the gap type share: a
// page gets the gap when at least one of its members is anchored in that
// file (`Symbol::location().file()`, the same repo-relative spelling as the
// record's key). An edge is anchored at its source, which is a member of the
// page (`communities`, `processes`), so members are the whole surface. A
// file that no page lists a member in is not dropped: it is returned in
// `unattributed`, so a caller can say the gap exists and belongs to no page.
//
// The other kinds of the record (skipped, quarantined, outside build
// targets, unlinked, feature gated) are not attributed here: skipped and
// quarantined files have no symbol in the graph, so no page to land on, and
// the skeleton schema v1 has no gap variant for the other three.

use std::collections::{BTreeMap, BTreeSet};

use crate::indexer::coverage::{CoverageKind, CoverageReport};

use super::skeleton::{CoverageGap, Page};

/// What `attribute_parse_incomplete` could not put on a page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageAttribution {
    /// `parse_incomplete` files no page lists a member in, sorted.
    pub unattributed: Vec<String>,
}

/// Add a `ParseIncomplete` gap to every page of `pages` that lists a member
/// defined in a `parse_incomplete` file of `report`. A page keeps the gaps
/// it already carries; one file is added at most once per page.
pub fn attribute_parse_incomplete(
    pages: &mut [Page],
    report: &CoverageReport,
) -> CoverageAttribution {
    let partial: BTreeSet<&str> = report
        .files
        .iter()
        .filter(|(_, cov)| cov.kind == CoverageKind::ParsePartial)
        .map(|(rel, _)| rel.as_str())
        .collect();
    let mut attributed: BTreeMap<&str, bool> = partial.iter().map(|f| (*f, false)).collect();
    for page in pages.iter_mut() {
        let files: BTreeSet<String> = page
            .members
            .iter()
            .map(|s| s.location.file())
            .filter(|f| partial.contains(f))
            .map(str::to_owned)
            .collect();
        for file in files {
            if let Some(seen) = attributed.get_mut(file.as_str()) {
                *seen = true;
            }
            let gap = CoverageGap::ParseIncomplete { file };
            if !page.coverage.contains(&gap) {
                page.coverage.push(gap);
            }
        }
    }
    CoverageAttribution {
        unattributed: attributed
            .into_iter()
            .filter(|(_, seen)| !seen)
            .map(|(file, _)| file.to_owned())
            .collect(),
    }
}

#[cfg(test)]
#[path = "coverage_tests.rs"]
mod tests;
