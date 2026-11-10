// clustering::impact_sites — the open call sites `get_impact` names (issue
// #445): an agent reading an incomplete `callers` list gets the file and line
// of every unresolved site that names the target, not only their count.
// Split from `impact_reasons.rs`, which would otherwise pass the §4.1
// 500-line cap.

use crate::language_provider::extract_file_prefix_or_self;

/// Cap on the open call sites `get_impact` names. `total` stays the true count,
/// so a listing shorter than it says how many were left out. A `query_graph` on
/// `CallSite` with `is_resolved = false` and the callee-name filter of
/// `impact_reasons::unresolved_name_filter` returns a superset of the rest: it
/// also holds the sites the other-owner filter leaves out of the count.
pub const UNRESOLVED_SITES_LISTED: usize = 50;

/// One open call site that names the target: where it is and how it is spelled
/// (issue #445). `file` is the prefix of the site's id, `line` is 1-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedSite {
    pub id: String,
    pub file: String,
    pub line: i64,
    pub callee_name: String,
    /// The recorded reason, or `not_recorded`.
    pub reason: String,
}

impl UnresolvedSite {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "file": self.file,
            "line": self.line,
            "callee_name": self.callee_name,
            "reason": self.reason,
        })
    }
}

/// The sites of `rows` (`[id, reason, callee_name, language, line, ...]`),
/// sorted by file, line and id, the first `UNRESOLVED_SITES_LISTED` kept.
pub(super) fn listed_sites(rows: &[Vec<String>]) -> Vec<UnresolvedSite> {
    let cell = |row: &[String], i: usize| row.get(i).cloned().unwrap_or_default();
    let mut sites: Vec<UnresolvedSite> = rows
        .iter()
        .map(|row| {
            let id = cell(row, 0);
            UnresolvedSite {
                file: extract_file_prefix_or_self(&id),
                line: cell(row, 4).parse().unwrap_or(0),
                callee_name: cell(row, 2),
                reason: crate::graph_store::callsite_reasons::reason_or_not_recorded(&cell(row, 1)),
                id,
            }
        })
        .collect();
    sites.sort_by(|a, b| (&a.file, a.line, &a.id).cmp(&(&b.file, b.line, &b.id)));
    sites.truncate(UNRESOLVED_SITES_LISTED);
    sites
}
