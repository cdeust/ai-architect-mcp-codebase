// graph_store::callsite_reasons: why each open call site is open (issue #393).
//
// Every `CallSite` the resolvers leave unresolved carries one reason from a
// closed set in `unresolved_reason`, and in `unresolved_detail` what the reason
// needs to be checked (the scope rule that declined, the number of candidates,
// the crate the callee comes from). A resolved site carries neither.
//
// The reasons split in two. Five name a site no static resolver of this graph
// should resolve: a file no Cargo target compiles, a macro, a macro that calls
// nothing, twins the build does not decide between, a callee proven to live
// outside the repository. The other five name a gap of the resolver, the ones
// worth improving. `unknown_callee` is the only reason that says nothing is
// known; `external_callee` is never given on absence alone.
//
// A graph written before the reasons existed has blank reasons on its open
// sites; `callsite_reason_form` (a marker row written after the resolution
// phases) says whether they were recorded. Reads never refuse: a blank on an
// unmarked graph reads `not_recorded`.

use std::collections::BTreeMap;

use lbug::{LogicalType, Value};

use super::cfg_twins::MARKER_TABLE;
use super::{cypher_str, GraphStore, BULK_BATCH_SIZE};

/// The version of the reasons and of the rules that write them. Bump it when
/// either changes what a graph stores.
pub(crate) const CALLSITE_REASON_FORM: u32 = 1;
const MARKER_ID: &str = "callsite_reason_form";

pub const REASON_OUTSIDE_TARGETS: &str = super::CALLSITE_UNRESOLVED_REASON_OUTSIDE_TARGETS;
pub const REASON_MACRO_SITE: &str = "macro_site";
pub const REASON_NOT_A_CALL: &str = "not_a_call";
pub const REASON_CFG_TWINS: &str = super::CALLSITE_UNRESOLVED_REASON_CFG_TWINS;
pub const REASON_EXTERNAL_CALLEE: &str = "external_callee";
pub const REASON_DECLINED_BY_SCOPE: &str = "declined_by_scope";
pub const REASON_AMBIGUOUS: &str = "ambiguous_candidates";
pub const REASON_NO_RECEIVER_TYPE: &str = "no_receiver_type";
pub const REASON_NOT_FOUND: &str = "not_found";
pub const REASON_UNKNOWN_CALLEE: &str = "unknown_callee";
/// An open site on a graph whose reasons were never recorded.
pub const REASON_NOT_RECORDED: &str = "not_recorded";

/// Reasons no static resolver of this graph should turn into an edge.
pub const BY_CONSTRUCTION: [&str; 5] = [
    REASON_OUTSIDE_TARGETS,
    REASON_MACRO_SITE,
    REASON_NOT_A_CALL,
    REASON_CFG_TWINS,
    REASON_EXTERNAL_CALLEE,
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

    /// Records that every open site of this graph carries its reason. Called
    /// after the resolution phases.
    pub fn write_callsite_reason_marker(&self) -> Result<(), String> {
        self.execute_query(&format!(
            "MERGE (m:{MARKER_TABLE} {{id: {}}}) SET m.value = {}",
            cypher_str(MARKER_ID),
            cypher_str(&CALLSITE_REASON_FORM.to_string())
        ))?;
        Ok(())
    }

    /// True when the graph carries the current reason marker. Read-only.
    pub fn has_callsite_reasons(&self) -> bool {
        if !self.has_node_table(MARKER_TABLE).unwrap_or(false) {
            return false;
        }
        let Ok(rows) = self.execute_query(&format!(
            "MATCH (m:{MARKER_TABLE} {{id: {}}}) RETURN m.value",
            cypher_str(MARKER_ID)
        )) else {
            return false;
        };
        rows.rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.parse::<u32>().ok())
            == Some(CALLSITE_REASON_FORM)
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
