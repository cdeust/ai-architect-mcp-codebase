//! Stage 3c — `get_impact`: reverse-dependency blast radius.
//!
//! Split from `process_impact_handlers` when the shared file crossed the
//! §4.1 500-line cap. The two halves share only the module's imports: the
//! `get_processes` handler answers "what execution flows exist", this one
//! answers "what depends on this target". Nothing calls across the seam.

use crate::clustering;
use crate::graph_cache;
use crate::graph_store;
use crate::response_budget;
use crate::search;
use crate::token_surface;
use serde_json::{json, Value};
use std::path::Path;

use super::impact_followups;
// ---------------------------------------------------------------------------
// Stage 3c — get_impact
// ---------------------------------------------------------------------------

pub(crate) fn run_get_impact(arguments: &Value) -> Value {
    let mut out = match do_get_impact(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "impact_failed", "message": msg
        }),
    };
    // fleet-watch#112: the tool's single exit, so the freshness receipt reaches
    // every answer — including a store-open or query failure, which is what an
    // in-progress re-index looks like from here.
    crate::graph_freshness::attach_from_arguments(&mut out, arguments);
    out
}

/// Scalar columns of a reverse-dependency handle, in tabular-projection order
/// (issue #56). Shared by all five impact sections (they are homogeneous):
/// callers/importers/users/implementors/references (references added by
/// issue #205).
pub(crate) const IMPACT_COLUMNS: &[&str] = &["qualified_name", "label", "confidence", "id"];

pub(crate) fn do_get_impact(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'qualified_name'")?;
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;

    // fleet-watch#19: resolve the caller's target to the stored form before the
    // impact query, exactly as get_symbol/get_context/check_gates do.
    // clustering::get_impact does only strict `n.id`/`n.qualified_name`
    // equality, and the parser strips the leading path component when building
    // qualified_names — so the README's `src/main.rs::foo` form matched no node
    // and returned empty callers/users with `epistemic: "exact"`, a misleading
    // empty result instead of an error. On a genuine miss return
    // `symbol_not_found` with suggestions, like the peer tools, rather than a
    // vacuous "exact" blast radius.
    //
    // The resolver is `search::resolve_impact_target`, not the symbol-only
    // `resolve_qualified_name` the peer tools use, because this tool also
    // answers File-target fan-in (issue #205) — see that module.
    let target = match search::resolve_impact_target(&store, qn) {
        Ok(t) => t,
        Err(nf) => return Ok(target_not_found_response(nf)),
    };
    Ok(impact_response(
        &store, arguments, args, graph_path, &target, offset,
    ))
}

/// The answer when neither a symbol nor a file in this graph matches the
/// caller's target.
///
/// The freshness receipt is NOT attached here: `run_get_impact` stamps it onto
/// whatever this tool returns, so every exit carries it without each exit
/// having to remember (fleet-watch#112 review round 3).
fn target_not_found_response(nf: search::SymbolNotFound) -> Value {
    json!({
        "stage": 3,
        "status": "error",
        "reason": "symbol_not_found",
        "message": format!("not found: {}", nf.input),
        "did_you_mean": nf.did_you_mean,
    })
}

/// Assembles the `get_impact` response for an already-resolved target.
///
/// Every graph lookup below keys off `target`, not the caller's raw input:
/// mixing the two is what made the impact query and the co-change section
/// disagree (review finding 5). `target.key` is still echoed to the caller as
/// `qualified_name` by `impact_envelope`, so the response names the node the
/// numbers actually describe.
fn impact_response(
    store: &graph_store::GraphStore,
    arguments: &Value,
    args: &serde_json::Map<String, Value>,
    graph_path: &Path,
    target: &search::ImpactTarget,
    offset: u64,
) -> Value {
    let mut impact = match clustering::get_impact(store, &target.key) {
        Ok(i) => i,
        Err(e) => {
            return json!({
                "stage": 3, "status": "error", "reason": "query_failed", "message": e
            })
        }
    };
    sort_impact_lists(&mut impact);

    let sections = paged_impact_sections(&impact, offset);
    let mut out = impact_envelope(&sections, &impact, &target.key, args);
    impact_followups::attach_cochange_section(&mut out, store, target.file.as_deref(), args);

    // Suggested follow-up traversals from this blast-radius result.
    out["next_steps"] = impact_followups::impact_next_steps(&impact, &target.key);

    impact_followups::attach_foreign_callers(&mut out, arguments, graph_path, &target.key);
    // `exact` is a completeness claim, and every other surface of this server
    // disclaims completeness. It must rest on a positive coverage signal rather
    // than on the absence of a negative one. source: ADR-9839.
    super::coverage_evidence::require_coverage_evidence(&mut out, graph_path);
    out
}

/// Cursor stability: each reverse-dependency list is assembled in
/// clustering::impact::reverse_dependents by iterating REL_TABLES and, per
/// table, the engine's unordered scan rows (no ORDER BY) — neither order is a
/// guaranteed-stable contract across calls. We impose a deterministic total
/// order at the read boundary by (qualified_name, id). `id` is the node's
/// unique graph id, so the pair is tie-free, giving a total order safe to page.
/// source: cursor-correctness requirement (response_budget::BoundedPage docs).
fn sort_impact_lists(impact: &mut clustering::ImpactResult) {
    let sort_nodes = |nodes: &mut Vec<clustering::ImpactNode>| {
        nodes.sort_by(|a, b| {
            a.qualified_name
                .cmp(&b.qualified_name)
                .then_with(|| a.id.cmp(&b.id))
        });
    };
    sort_nodes(&mut impact.callers);
    sort_nodes(&mut impact.importers);
    sort_nodes(&mut impact.users);
    sort_nodes(&mut impact.implementors);
    sort_nodes(&mut impact.references);
}

/// Serializes reverse-dependency endpoints as re-queryable handles so the
/// caller can keep traversing through MCP (get_symbol/get_context on `id`)
/// rather than receiving a flattened terminal digest. `confidence` < 1.0
/// marks a heuristically-resolved dependency edge.
fn impact_handles(nodes: &[clustering::ImpactNode]) -> Vec<Value> {
    nodes
        .iter()
        .map(|n| {
            json!({
                "id": n.id,
                "qualified_name": n.qualified_name,
                "label": n.label,
                "confidence": format!("{:.2}", n.confidence),
            })
        })
        .collect()
}

/// The five bounded reverse-dependency sections of one `get_impact` response,
/// plus the pre-truncation totals, grouped so the envelope builder takes one
/// parameter object (§4.4).
struct ImpactSections {
    offset: u64,
    callers: response_budget::BoundedPage,
    importers: response_budget::Bounded,
    users: response_budget::Bounded,
    implementors: response_budget::Bounded,
    references: response_budget::Bounded,
    dependents_total: usize,
    references_total_full: usize,
}

/// Pages the primary list and byte-bounds the secondary ones.
///
/// `callers` is the PRIMARY list and the only one the `offset` cursor pages:
/// a caller can page callers to exhaustion via `next_offset`. The remaining
/// sections (importers/users/implementors/references) are bounded SUMMARIES —
/// they start from index 0 and are byte-capped, not cursored. This is reported
/// honestly (see `secondary_lists_paged: false`) rather than inventing a
/// multi-cursor scheme. To page a secondary blast-radius dimension at scale,
/// query it directly via query_graph (which carries its own ORDER BY + offset).
///
/// `dependents_total` is computed from the FULL, pre-truncation counts so the
/// caller always sees the true blast-radius size even when a section is cut.
/// Deliberately CODE-ONLY (callers/importers/users/implementors) — the
/// `counts` object splits code vs. reference fan-in explicitly (issue #205),
/// so this field's meaning is unchanged for existing callers. `references`
/// (doc/script cross-references, issue #205) gets the same bounded-summary
/// treatment, kept in its own section since it is not a code dependency.
fn paged_impact_sections(impact: &clustering::ImpactResult, offset: u64) -> ImpactSections {
    let budget = response_budget::per_section_chars();
    ImpactSections {
        offset,
        callers: response_budget::bound_values_paged(
            impact_handles(&impact.callers),
            offset,
            budget,
        ),
        importers: response_budget::bound_values(impact_handles(&impact.importers), budget),
        users: response_budget::bound_values(impact_handles(&impact.users), budget),
        implementors: response_budget::bound_values(impact_handles(&impact.implementors), budget),
        references: response_budget::bound_values(impact_handles(&impact.references), budget),
        dependents_total: impact.callers.len()
            + impact.importers.len()
            + impact.users.len()
            + impact.implementors.len(),
        references_total_full: impact.references.len(),
    }
}

/// Builds the `get_impact` response envelope from the bounded sections.
///
/// Token-surface shaping (issue #56): each reverse-dependency section renders
/// under the shared detail/format. ids → bare qualified names; tabular →
/// columns-once rows (one `columns` header covers the homogeneous sections).
/// The `counts` object reports code vs. reference fan-in together (issue
/// #205) so a caller does not have to sum sections itself. `epistemic` is the
/// honesty marker: `lower-bound` means real impact may exceed what is shown —
/// dynamic dispatch and/or heuristically-resolved edges; `epistemic_reasons`
/// names the carriers of uncertainty (source: epistemic module).
fn impact_envelope(
    sections: &ImpactSections,
    impact: &clustering::ImpactResult,
    qn: &str,
    args: &serde_json::Map<String, Value>,
) -> Value {
    let views = SectionViews::render(sections, args);
    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_impact",
        "qualified_name": qn,
        "communities": impact.communities,
        "communities_affected": impact.communities.len(),
        "processes": impact.processes,
        "processes_affected": impact.processes.len(),
        "detail": views.callers.detail,
        "format": views.callers.format,
        "callers": views.callers.value,
        "callers_total": sections.callers.total_count,
        "offset": sections.offset,
        "primary_list": "callers",
        "secondary_lists_paged": false,
        "importers": views.importers.value,
        "importers_total": sections.importers.total_count,
        "users": views.users.value,
        "users_total": sections.users.total_count,
        "implementors": views.implementors.value,
        "implementors_total": sections.implementors.total_count,
        "references": views.references.value,
        "references_total": sections.references.total_count,
        "dependents_total": sections.dependents_total,
        "counts": {
            "code": sections.dependents_total,
            "references": sections.references_total_full,
        },
        "truncated": views.any_truncated(sections),
        "epistemic": impact.epistemic.as_str(),
        "epistemic_reasons": impact.epistemic_reasons,
        "unresolved_callsites_naming_target": impact.unresolved_callsites_naming_target,
        "unresolved_callsites_outside_targets": impact.unresolved_callsites_outside_targets,
    });
    if views.callers.columns.is_some() {
        // One header covers all homogeneous sections.
        out["columns"] = json!(IMPACT_COLUMNS);
    }
    if let Some(next) = sections.callers.next_offset {
        out["next_offset"] = json!(next);
    }
    out
}

/// The five reverse-dependency sections rendered under one detail/format
/// choice. Split out of `impact_envelope` so the envelope reads as the shape
/// it emits rather than as rendering plus shape (§4.2).
struct SectionViews {
    callers: token_surface::ListView,
    importers: token_surface::ListView,
    users: token_surface::ListView,
    implementors: token_surface::ListView,
    references: token_surface::ListView,
}

impl SectionViews {
    fn render(sections: &ImpactSections, args: &serde_json::Map<String, Value>) -> Self {
        let detail = token_surface::parse_detail(args);
        let format = token_surface::parse_format(args);
        let render = |items: &[Value]| {
            token_surface::render_list(items, IMPACT_COLUMNS, "qualified_name", &detail, &format)
        };
        SectionViews {
            callers: render(&sections.callers.items),
            importers: render(&sections.importers.items),
            users: render(&sections.users.items),
            implementors: render(&sections.implementors.items),
            references: render(&sections.references.items),
        }
    }

    /// True when ANY section was cut, so the envelope's `truncated` flag
    /// covers the whole response rather than its primary list alone.
    fn any_truncated(&self, sections: &ImpactSections) -> bool {
        sections.callers.truncated
            || sections.importers.truncated
            || sections.users.truncated
            || sections.implementors.truncated
            || sections.references.truncated
    }
}
