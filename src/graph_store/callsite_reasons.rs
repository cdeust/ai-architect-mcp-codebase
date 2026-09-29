// graph_store::callsite_reasons: why each open call site is open (issue #393).
//
// Every `CallSite` the resolvers leave unresolved carries one reason from a
// closed set in `unresolved_reason`, and in `unresolved_detail` what the reason
// needs to be checked (the scope rule that declined, the number of candidates,
// the crate the callee comes from). A resolved site carries neither.
//
// The reasons split in two. Six name a site no static resolver of this graph
// should resolve: a file no Cargo target compiles, a macro, a macro that calls
// nothing, twins the build does not decide between, a callee proven to live
// outside the repository, a C or C++ call through a function pointer (form 2,
// issue #401). The other five name a gap of the resolver, the ones
// worth improving. `unknown_callee` is the only reason that says nothing is
// known; `external_callee` is never given on absence alone.
//
// A graph written before the reasons existed has blank reasons on its open
// sites; `callsite_reason_form` (a marker row written after the resolution
// phases) says whether they were recorded. Reads never refuse: a blank on an
// unmarked graph reads `not_recorded`.
//
// The reason marker vouches for EVERY `CallSite` row, but a resolve pass only
// re-evaluates the rows, it never re-parses a file. So it is written only on a
// graph whose rows were all produced under the current parser form: the
// `callsite_rows_form` marker, written at the end of a full index like
// `body_kind_form` (a full index clears every marker at its start). An
// incremental refresh of an older graph leaves unchanged files with rows of the
// old form, so the graph carries no rows marker and `reasons_recorded` stays
// false until a full index (issue #408).

use std::collections::BTreeMap;

use lbug::{LogicalType, Value};

use super::cfg_twins::MARKER_TABLE;
use super::{cypher_str, GraphStore, BULK_BATCH_SIZE};

/// The version of the reasons and of the rules that write them. Bump it when
/// either changes what a graph stores. 3: the marker is written only over rows of
/// the current form (issue #408), so a form-2 marker written over old rows is no
/// longer believed.
pub(crate) const CALLSITE_REASON_FORM: u32 = 3;
const MARKER_ID: &str = "callsite_reason_form";

/// The version of the parser output a `CallSite` row records (`callee_shape`,
/// the indirect sites). Bump it when a parser change alters what a row holds,
/// so a graph written before stops vouching for its reasons.
pub(crate) const CALLSITE_ROWS_FORM: u32 = 1;
const ROWS_MARKER_ID: &str = "callsite_rows_form";

pub const REASON_OUTSIDE_TARGETS: &str = super::CALLSITE_UNRESOLVED_REASON_OUTSIDE_TARGETS;
pub const REASON_MACRO_SITE: &str = "macro_site";
pub const REASON_NOT_A_CALL: &str = "not_a_call";
pub const REASON_CFG_TWINS: &str = super::CALLSITE_UNRESOLVED_REASON_CFG_TWINS;
pub const REASON_EXTERNAL_CALLEE: &str = "external_callee";
/// A C or C++ call through a function pointer (issue #401).
pub const REASON_INDIRECT_CALL: &str = "indirect_call";
pub const REASON_DECLINED_BY_SCOPE: &str = "declined_by_scope";
pub const REASON_AMBIGUOUS: &str = "ambiguous_candidates";
pub const REASON_NO_RECEIVER_TYPE: &str = "no_receiver_type";
pub const REASON_NOT_FOUND: &str = "not_found";
pub const REASON_UNKNOWN_CALLEE: &str = "unknown_callee";
/// An open site on a graph whose reasons were never recorded.
pub const REASON_NOT_RECORDED: &str = "not_recorded";

/// Reasons no static resolver of this graph should turn into an edge.
pub const BY_CONSTRUCTION: [&str; 6] = [
    REASON_OUTSIDE_TARGETS,
    REASON_MACRO_SITE,
    REASON_NOT_A_CALL,
    REASON_CFG_TWINS,
    REASON_EXTERNAL_CALLEE,
    REASON_INDIRECT_CALL,
];

/// Reasons that name a gap of the resolver.
pub const IMPROVABLE: [&str; 5] = [
    REASON_DECLINED_BY_SCOPE,
    REASON_AMBIGUOUS,
    REASON_NO_RECEIVER_TYPE,
    REASON_NOT_FOUND,
    REASON_UNKNOWN_CALLEE,
];

/// One open site and why: `(id, reason, detail)`.
pub type SiteReasonRow = (String, &'static str, String);

/// The open call sites of a graph, by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnresolvedSiteSummary {
    pub total: u64,
    pub by_reason: BTreeMap<String, u64>,
    /// True when the graph carries the current `callsite_reason_form` marker.
    pub reasons_recorded: bool,
}

impl UnresolvedSiteSummary {
    /// Open sites whose reason is one of `reasons`.
    pub fn sum_of(&self, reasons: &[&str]) -> u64 {
        reasons.iter().filter_map(|r| self.by_reason.get(*r)).sum()
    }
}

/// Reasons counted one per item; a blank reads `not_recorded`.
pub(crate) fn count_by_reason(reasons: impl Iterator<Item = String>) -> BTreeMap<String, u64> {
    let mut by_reason = BTreeMap::new();
    for reason in reasons {
        *by_reason
            .entry(reason_or_not_recorded(&reason))
            .or_insert(0) += 1;
    }
    by_reason
}

fn reason_or_not_recorded(reason: &str) -> String {
    if reason.is_empty() {
        REASON_NOT_RECORDED.to_string()
    } else {
        reason.to_string()
    }
}

impl GraphStore {
    /// Adds the two reason columns to a graph written before they existed.
    pub(crate) fn ensure_callsite_reason_columns(&self) -> Result<(), String> {
        self.ensure_node_column("CallSite", "unresolved_reason", "STRING DEFAULT ''")?;
        self.ensure_node_column("CallSite", "unresolved_detail", "STRING DEFAULT ''")?;
        Ok(())
    }

    /// Writes each row's reason and detail on its site, unless the site is
    /// resolved: a later pass (the language server) may have resolved a site a
    /// static pass cannot, and a resolved site carries no reason.
    pub(crate) fn write_callsite_reasons(&self, rows: &[SiteReasonRow]) -> Result<(), String> {
        if rows.is_empty() {
            return Ok(());
        }
        self.ensure_callsite_reason_columns()?;
        let mut grouped: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
        for (id, reason, detail) in rows {
            grouped
                .entry((reason, detail.as_str()))
                .or_default()
                .push(id.as_str());
        }
        for ((reason, detail), ids) in grouped {
            let cypher = format!(
                "UNWIND $rows AS rid MATCH (n:CallSite {{id: rid}}) \
                 WHERE n.is_resolved IS NULL OR n.is_resolved = false \
                 SET n.unresolved_reason = {}, n.unresolved_detail = {}",
                cypher_str(reason),
                cypher_str(detail)
            );
            self.run_id_chunks(&cypher, &ids)?;
        }
        Ok(())
    }

    /// Rewrites the reason of every open site of `ids` to `external_callee`
    /// with `detail`, when its reason names a resolver gap (`IMPROVABLE`): the
    /// language server showed the callee's definition outside the analyzed root.
    /// A reason that says more (a file outside the targets, twins, a macro, a
    /// value built rather than called) is kept. The next static pass writes its
    /// own reasons again; the next language-server pass restores this one.
    pub(crate) fn promote_callsite_reason_to_external(
        &self,
        ids: &[&str],
        detail: &str,
    ) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        self.ensure_callsite_reason_columns()?;
        let gaps = IMPROVABLE
            .iter()
            .map(|r| cypher_str(r))
            .collect::<Vec<_>>()
            .join(", ");
        let cypher = format!(
            "UNWIND $rows AS rid MATCH (n:CallSite {{id: rid}}) \
             WHERE (n.is_resolved IS NULL OR n.is_resolved = false) \
             AND n.unresolved_reason IN [{gaps}] \
             SET n.unresolved_reason = {}, n.unresolved_detail = {}",
            cypher_str(REASON_EXTERNAL_CALLEE),
            cypher_str(detail)
        );
        self.run_id_chunks(&cypher, ids)
    }

    /// Empties the reason and detail of every site in `ids`: they resolved.
    pub(crate) fn clear_callsite_reasons(&self, ids: &[&str]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        self.ensure_callsite_reason_columns()?;
        self.run_id_chunks(
            "UNWIND $rows AS rid MATCH (n:CallSite {id: rid}) \
             SET n.unresolved_reason = '', n.unresolved_detail = ''",
            ids,
        )
    }

    fn run_id_chunks(&self, cypher: &str, ids: &[&str]) -> Result<(), String> {
        for chunk in ids.chunks(BULK_BATCH_SIZE) {
            let values = chunk
                .iter()
                .map(|id| Value::String((*id).to_string()))
                .collect();
            self.run_prepared(cypher, Value::List(LogicalType::String, values))?;
        }
        Ok(())
    }

    /// Records that every `CallSite` row of this graph was written under the
    /// current parser form. Called at the END of a successful full index (the
    /// start clears every marker row).
    pub fn write_callsite_rows_marker(&self) -> Result<(), String> {
        self.write_marker(ROWS_MARKER_ID, CALLSITE_ROWS_FORM)
    }

    /// Records that every open site of this graph carries its reason. Called
    /// after the resolution phases. Only a graph whose rows are of the current
    /// form is vouched for; on any other the marker is withdrawn, so a marker a
    /// graph carries never covers rows an incremental refresh left behind
    /// (issue #408).
    pub fn write_callsite_reason_marker(&self) -> Result<(), String> {
        if !self.has_callsite_rows() {
            self.execute_query(&format!(
                "MATCH (m:{MARKER_TABLE} {{id: {}}}) DELETE m",
                cypher_str(MARKER_ID)
            ))?;
            return Ok(());
        }
        self.write_marker(MARKER_ID, CALLSITE_REASON_FORM)
    }

    fn write_marker(&self, id: &str, form: u32) -> Result<(), String> {
        self.execute_query(&format!(
            "MERGE (m:{MARKER_TABLE} {{id: {}}}) SET m.value = {}",
            cypher_str(id),
            cypher_str(&form.to_string())
        ))?;
        Ok(())
    }

    /// True when every row of the graph was written under the current parser
    /// form. Read-only; a failed check counts as absent.
    pub fn has_callsite_rows(&self) -> bool {
        self.marker_form(ROWS_MARKER_ID) == Some(CALLSITE_ROWS_FORM)
    }

    /// True when the graph carries the current reason marker. Read-only.
    pub fn has_callsite_reasons(&self) -> bool {
        self.marker_form(MARKER_ID) == Some(CALLSITE_REASON_FORM)
    }

    /// A marker row read as a form number; a failed read counts as absent.
    fn marker_form(&self, id: &str) -> Option<u32> {
        self.marker_value(id).ok().flatten()?.parse().ok()
    }

    /// Every open call site counted by reason. Read-only: a graph without the
    /// reason column counts every open site `not_recorded`.
    pub fn unresolved_site_summary(&self) -> Result<UnresolvedSiteSummary, String> {
        let open = "cs.is_resolved IS NULL OR cs.is_resolved = false";
        let has_column = self.node_column_exists("CallSite", "unresolved_reason")?;
        let by_reason = if has_column {
            let rows = self.execute_query(&format!(
                "MATCH (cs:CallSite) WHERE {open} \
                 RETURN cs.unresolved_reason, count(cs)"
            ))?;
            let mut by_reason = BTreeMap::new();
            for row in rows.rows.iter().filter(|r| r.len() >= 2) {
                let n: u64 = row[1].parse().unwrap_or(0);
                *by_reason
                    .entry(reason_or_not_recorded(&row[0]))
                    .or_insert(0) += n;
            }
            by_reason
        } else {
            let rows = self.execute_query(&format!(
                "MATCH (cs:CallSite) WHERE {open} RETURN count(cs)"
            ))?;
            let n: u64 = rows
                .rows
                .first()
                .and_then(|r| r.first())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            BTreeMap::from([(REASON_NOT_RECORDED.to_string(), n)])
        };
        let by_reason: BTreeMap<String, u64> =
            by_reason.into_iter().filter(|(_, n)| *n > 0).collect();
        Ok(UnresolvedSiteSummary {
            total: by_reason.values().sum(),
            by_reason,
            reasons_recorded: self.has_callsite_reasons(),
        })
    }
}
