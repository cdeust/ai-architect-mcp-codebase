//! Stage 3c — `get_impact` follow-up sections: co-change partners (issue
//! #58), cross-repo foreign callers, and the `next_steps` suggestions.
//!
//! Split from `impact.rs` (issue #284, lot 5) when this lot's
//! outside-target hint would have pushed that file past the §4.1 500-line
//! cap — a clean concern boundary: `impact.rs` assembles the core
//! reverse-dependency envelope, this file assembles the "what else might
//! help" sections layered on top of it. Pure move except `impact_next_steps`,
//! whose unresolved-callsite hint gained the outside-target clause.

use crate::bridge;
use crate::clustering;
use crate::epistemic;
use crate::graph_store;
use crate::response_budget;
use crate::token_surface;
use serde_json::{json, Value};
use std::path::Path;

/// Columns of a co-change partner row (issue #58) in get_impact.
pub(crate) const COCHANGE_PARTNER_COLUMNS: &[&str] =
    &["file", "cochange_count", "coupling", "jaccard"];

/// The FILE_CHANGES_WITH partners of `file` (issue #58), strongest coupling
/// first, as homogeneous objects ready for the token surface. Best-effort: an
/// empty graph or a graph mined without cochange yields an empty list.
pub(crate) fn cochange_partners(store: &graph_store::GraphStore, file: &str) -> Vec<Value> {
    // Undirected: FILE_CHANGES_WITH stores one edge per pair (a<b), so match both
    // directions to find every partner of `file`.
    let q = format!(
        "MATCH (f:File)-[r:FILE_CHANGES_WITH]-(g:File) WHERE f.id = {} \
         RETURN g.id, r.cochange_count, r.coupling, r.jaccard \
         ORDER BY r.coupling DESC, g.id",
        graph_store::cypher_str(file)
    );
    let qr = match store.execute_query(&q) {
        Ok(qr) => qr,
        Err(_) => return Vec::new(),
    };
    qr.rows
        .iter()
        .filter(|row| row.len() >= 4)
        .map(|row| {
            json!({
                "file": row[0],
                "cochange_count": row[1].parse::<i64>().unwrap_or(0),
                "coupling": row[2],
                "jaccard": row[3],
            })
        })
        .collect()
}

/// Issue #58: the symbol's FILE co-change partners are impact candidates the
/// static call graph cannot see (files that historically change together —
/// the architect agent's churning-pairs signal). Added as a section under the
/// same detail/format surface. Partners have their own shape ({file,
/// cochange_count, coupling, jaccard}), so they render with their own
/// columns/id — NOT the impact columns.
///
/// `file` is the resolved `File.id` from `search::resolve_impact_target`, not
/// the caller's input split on `::`. Deriving it here from the raw input is
/// what made this section return `[]` for the stored qualified-name form
/// (review finding 5): `main.rs::foo` yielded `main.rs`, which is no
/// `File.id`. An unresolvable file yields an empty section, as before.
pub(super) fn attach_cochange_section(
    out: &mut Value,
    store: &graph_store::GraphStore,
    file: Option<&str>,
    args: &serde_json::Map<String, Value>,
) {
    let partners = file
        .map(|f| cochange_partners(store, f))
        .unwrap_or_default();
    let partner_view = token_surface::render_list(
        &partners,
        COCHANGE_PARTNER_COLUMNS,
        "file",
        &token_surface::parse_detail(args),
        &token_surface::parse_format(args),
    );
    out["cochange_partners"] = partner_view.value;
    out["cochange_partners_total"] = json!(partners.len());
    if partner_view.columns.is_some() {
        out["cochange_partner_columns"] = json!(COCHANGE_PARTNER_COLUMNS);
    }
}

/// Cross-repo bridge: when sibling graphs are supplied, also surfaces callers
/// that live in OTHER repos. These are reported in their own section (not
/// merged into the local `callers`/`dependents_total`) so blast radius keeps
/// local and foreign impact distinct. Absent the arg this is a no-op.
/// source: cross-repo bridge spec (bridge module).
///
/// Cross-repo edges are name-matched without a shared linker, so any foreign
/// caller makes the blast radius a lower bound (and stays one even if the
/// local set was exact). source: epistemic module contract.
pub(super) fn attach_foreign_callers(
    out: &mut Value,
    arguments: &Value,
    graph_path: &Path,
    qn: &str,
) {
    let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
    if siblings.is_empty() {
        return;
    }
    let foreign = bridge::foreign_callers(&siblings, bridge::last_segment(qn));
    let handles: Vec<Value> = foreign.iter().map(|f| f.to_json()).collect();
    let foreign_page = response_budget::bound_values(handles, response_budget::per_section_chars());
    out["foreign_callers"] = json!(foreign_page.items);
    out["foreign_callers_total"] = json!(foreign.len());
    out["foreign_callers_paged"] = json!(false);
    if !siblings.skipped.is_empty() {
        out["sibling_graphs_skipped"] = json!(siblings.skipped);
    }
    if !foreign.is_empty() {
        out["epistemic"] = json!(epistemic::Boundary::LowerBound.as_str());
        if let Some(reasons) = out["epistemic_reasons"].as_array_mut() {
            reasons.push(json!(format!(
                "{} cross-repo caller(s) were matched by symbol name across \
                 sibling graphs without a shared linker (confidence 0.50); \
                 foreign blast radius is heuristic",
                foreign.len()
            )));
        }
    }
}

/// Suggests the natural follow-up tool calls after a `get_impact` result, so a
/// caller continues traversing the graph rather than stopping at the digest.
/// Hints are graph-grounded (only suggested when the corresponding dimension is
/// non-empty / relevant), never speculative.
pub(crate) fn impact_next_steps(impact: &clustering::ImpactResult, qn: &str) -> Value {
    let mut steps = Vec::new();
    if !impact.callers.is_empty() {
        steps.push(
            "inspect a caller's own blast radius: get_impact on a `callers[].qualified_name`"
                .to_string(),
        );
    }
    // issue #283 (a): an empty `callers` list reads as "no callers" unless a
    // caller also sees `unresolved_callsites_naming_target > 0` — this hint
    // makes the distinguishing action explicit instead of leaving the caller
    // to notice the structured field on their own. Issue #284 (lot 5): when
    // some of those sites are attributed outside the compiled Cargo targets,
    // names that so the caller does not waste a run on `lsp_resolve` for the
    // sites it can never reach.
    if impact.callers.is_empty() && impact.unresolved_callsites_naming_target > 0 {
        let mut hint = format!(
            "{} call site(s) name this symbol but none resolved — run analyze_codebase \
             with lsp: true (Rust receiver calls need it) or check \
             query_graph(graph=\"missed\") for files the language server cannot see",
            impact.unresolved_callsites_naming_target
        );
        if impact.unresolved_callsites_outside_targets > 0 {
            hint.push_str(&format!(
                "; {} of them sit outside the compiled Cargo targets and lsp_resolve will \
                 never reach them — see query_graph(graph=\"missed\").coverage.outside_build_targets",
                impact.unresolved_callsites_outside_targets
            ));
        }
        steps.push(hint);
    }
    if impact.epistemic == epistemic::Boundary::LowerBound {
        steps.push(format!(
            "this is a lower bound — run get_context on '{qn}' to review its \
             interface relationships, or lsp_resolve to tighten dynamic-dispatch edges"
        ));
    }
    if !impact.implementors.is_empty() {
        steps.push(
            "review an implementor directly: get_symbol on an `implementors[].qualified_name`"
                .to_string(),
        );
    }
    json!(steps)
}

#[cfg(test)]
mod impact_next_steps_tests {
    use super::*;
    use crate::clustering::{ImpactNode, ImpactResult};

    fn caller_node(qualified_name: &str) -> ImpactNode {
        ImpactNode {
            id: qualified_name.to_string(),
            qualified_name: qualified_name.to_string(),
            label: "Function".to_string(),
            confidence: 1.0,
        }
    }

    fn base_impact() -> ImpactResult {
        ImpactResult {
            communities: Vec::new(),
            processes: Vec::new(),
            callers: Vec::new(),
            importers: Vec::new(),
            users: Vec::new(),
            implementors: Vec::new(),
            references: Vec::new(),
            epistemic: epistemic::Boundary::Exact,
            epistemic_reasons: Vec::new(),
            unresolved_callsites_naming_target: 0,
            unresolved_callsites_outside_targets: 0,
        }
    }

    fn has_unresolved_callsite_hint(steps: &Value) -> bool {
        steps
            .as_array()
            .expect("next_steps is a JSON array")
            .iter()
            .any(|s| s.as_str().unwrap_or_default().contains("name this symbol"))
    }

    /// Positive branch of the `callers.is_empty() && unresolved_count > 0`
    /// guard (issue #283 (a)): no callers, but unresolved call sites name the
    /// target -> the hint must appear.
    #[test]
    fn hints_unresolved_callsites_when_callers_empty() {
        let mut impact = base_impact();
        impact.unresolved_callsites_naming_target = 3;

        let steps = impact_next_steps(&impact, "crate::foo::bar");

        assert!(
            has_unresolved_callsite_hint(&steps),
            "expected the unresolved-callsite hint, got: {steps}"
        );
    }

    /// Negative branch of the same guard, previously untested: `callers` is
    /// non-empty even though unresolved call sites also name the target. A
    /// mutant that deletes the `impact.callers.is_empty()` conjunct (or the
    /// guard entirely) would surface the hint here too; this test fails
    /// against that mutant and passes only with the guard intact.
    #[test]
    fn omits_unresolved_callsites_hint_when_callers_non_empty() {
        let mut impact = base_impact();
        impact.callers = vec![caller_node("crate::foo::caller")];
        impact.unresolved_callsites_naming_target = 3;

        let steps = impact_next_steps(&impact, "crate::foo::bar");

        assert!(
            !has_unresolved_callsite_hint(&steps),
            "unresolved-callsite hint must not fire while callers is non-empty, got: {steps}"
        );
    }

    /// Issue #284 (lot 5): when some of the unresolved sites are attributed
    /// outside the compiled Cargo targets, the hint must name that — steering
    /// the caller away from re-running `lsp_resolve` on sites it can never
    /// reach.
    #[test]
    fn hints_outside_targets_when_present() {
        let mut impact = base_impact();
        impact.unresolved_callsites_naming_target = 5;
        impact.unresolved_callsites_outside_targets = 5;

        let steps = impact_next_steps(&impact, "crate::foo::bar");

        let joined = steps
            .as_array()
            .expect("array")
            .iter()
            .map(|s| s.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            joined.contains("outside the compiled Cargo targets"),
            "expected the outside-targets clause, got: {joined}"
        );
    }

    /// The outside-targets clause must not appear when no site is attributed
    /// outside the compiled targets — a mutant that always appends it would
    /// fail this test.
    #[test]
    fn omits_outside_targets_clause_when_zero() {
        let mut impact = base_impact();
        impact.unresolved_callsites_naming_target = 3;
        impact.unresolved_callsites_outside_targets = 0;

        let steps = impact_next_steps(&impact, "crate::foo::bar");

        let joined = steps
            .as_array()
            .expect("array")
            .iter()
            .map(|s| s.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            !joined.contains("outside the compiled Cargo targets"),
            "clause must be absent when nothing is attributed outside targets, got: {joined}"
        );
    }
}
