//! Stage 3c — `get_processes` / `get_impact` handler logic.
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use crate::bridge;
use crate::epistemic;
use crate::token_surface;
use serde_json::{json, Value};
use std::path::Path;

use crate::clustering;
use crate::graph_cache;
use crate::graph_store;
use crate::response_budget;

// ---------------------------------------------------------------------------
// Stage 3c — get_processes
// ---------------------------------------------------------------------------

pub(crate) fn run_get_processes(arguments: &Value) -> Value {
    match do_get_processes(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "processes_failed", "message": msg
        }),
    }
}

pub(crate) fn do_get_processes(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;
    let mut processes = clustering::get_processes(&store)?;

    // Cursor stability: get_processes runs `MATCH (p:Process) RETURN ...` with no
    // ORDER BY (clustering::process::get_processes), so the Ladybug scan order is
    // an engine implementation detail and is NOT guaranteed identical across
    // calls — paging over it would skip/duplicate rows. We impose a deterministic
    // total order here, at the read boundary, by (name, entry_point). Process
    // names are entry-point-derived and entry_point is a node id, so the pair is
    // unique, giving a total (tie-free) order. source: cursor-correctness
    // requirement (response_budget::BoundedPage docs).
    processes.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.entry_point.cmp(&b.entry_point))
    });

    let procs: Vec<Value> = processes
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "entry_point": p.entry_point,
                "entry_kind": p.entry_kind,
                "depth": p.depth,
                "node_count": p.node_count,
            })
        })
        .collect();

    // Page the array by serialized size from `offset` so a graph with thousands
    // of entry points cannot blow the host's MCP tool-result cap, and a client
    // can retrieve the full list in budget-sized pages via `next_offset`.
    let page =
        response_budget::bound_values_paged(procs, offset, response_budget::per_section_chars());

    // Token-surface shaping (issue #56): ids → bare process names; tabular →
    // columns-once rows. See token_surface.
    let detail = token_surface::parse_detail(args);
    let format = token_surface::parse_format(args);
    let view = token_surface::render_list(&page.items, PROCESS_COLUMNS, "name", &detail, &format);

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_processes",
        "process_count": page.items.len(),
        "total_count": page.total_count,
        "offset": offset,
        "truncated": page.truncated,
        "detail": view.detail,
        "format": view.format,
        "processes": view.value,
    });
    if let Some(cols) = view.columns {
        out["columns"] = cols;
    }
    if let Some(next) = page.next_offset {
        out["next_offset"] = json!(next);
    }
    Ok(out)
}

/// Scalar columns of a process row, in tabular-projection order (issue #56).
pub(crate) const PROCESS_COLUMNS: &[&str] =
    &["name", "entry_point", "entry_kind", "depth", "node_count"];

// ---------------------------------------------------------------------------
// Stage 3c — get_impact
// ---------------------------------------------------------------------------

pub(crate) fn run_get_impact(arguments: &Value) -> Value {
    match do_get_impact(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "impact_failed", "message": msg
        }),
    }
}

/// Scalar columns of a reverse-dependency handle, in tabular-projection order
/// (issue #56). Shared by all five impact sections (they are homogeneous):
/// callers/importers/users/implementors/references (references added by
/// issue #205).
pub(crate) const IMPACT_COLUMNS: &[&str] = &["qualified_name", "label", "confidence", "id"];

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
    let mut impact = clustering::get_impact(&store, qn)?;

    // Cursor stability: each reverse-dependency list is assembled in
    // clustering::impact::reverse_dependents by iterating REL_TABLES and, per
    // table, the engine's unordered scan rows (no ORDER BY) — neither order is a
    // guaranteed-stable contract across calls. We impose a deterministic total
    // order at the read boundary by (qualified_name, id). `id` is the node's
    // unique graph id, so the pair is tie-free, giving a total order safe to page.
    // source: cursor-correctness requirement (response_budget::BoundedPage docs).
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

    // Serialize reverse-dependency endpoints as re-queryable handles so the
    // caller can keep traversing through MCP (get_symbol/get_context on `id`)
    // rather than receiving a flattened terminal digest.
    let to_handles = |nodes: &[clustering::ImpactNode]| -> Vec<Value> {
        nodes
            .iter()
            .map(|n| {
                json!({
                    "id": n.id,
                    "qualified_name": n.qualified_name,
                    "label": n.label,
                    // Confidence of the edge linking this dependent to the
                    // target; < 1.0 marks a heuristically-resolved dependency.
                    "confidence": format!("{:.2}", n.confidence),
                })
            })
            .collect()
    };
    // dependents_total is computed from the FULL, pre-truncation counts so the
    // caller always sees the true blast-radius size even when a section is cut.
    // Deliberately CODE-ONLY (callers/importers/users/implementors) — the
    // `counts` object below is where code vs. reference fan-in is split out
    // explicitly (issue #205), so this field's meaning is unchanged for
    // existing callers.
    let dependents_total = impact.callers.len()
        + impact.importers.len()
        + impact.users.len()
        + impact.implementors.len();
    let references_total_full = impact.references.len();

    // `callers` is the PRIMARY list and the only one the `offset` cursor pages:
    // a caller can page callers to exhaustion via `next_offset`. The remaining
    // three sections (importers/users/implementors) are bounded SUMMARIES — they
    // start from index 0 and are byte-capped, not cursored. This is reported
    // honestly (see `secondary_lists_paged: false`) rather than inventing a
    // multi-cursor scheme. To page a secondary blast-radius dimension at scale,
    // query it directly via query_graph (which carries its own ORDER BY + offset).
    let callers = response_budget::bound_values_paged(
        to_handles(&impact.callers),
        offset,
        response_budget::per_section_chars(),
    );
    let importers = response_budget::bound_values(
        to_handles(&impact.importers),
        response_budget::per_section_chars(),
    );
    let users = response_budget::bound_values(
        to_handles(&impact.users),
        response_budget::per_section_chars(),
    );
    let implementors = response_budget::bound_values(
        to_handles(&impact.implementors),
        response_budget::per_section_chars(),
    );
    // Doc/script cross-references (issue #205) — same bounded-summary
    // treatment as importers/users/implementors, kept in its own section
    // since it is not a code dependency.
    let references = response_budget::bound_values(
        to_handles(&impact.references),
        response_budget::per_section_chars(),
    );

    let any_truncated = callers.truncated
        || importers.truncated
        || users.truncated
        || implementors.truncated
        || references.truncated;

    // Token-surface shaping (issue #56): render each reverse-dependency section
    // under the shared detail/format. ids → bare qualified names; tabular →
    // columns-once rows (one `columns` header covers all four sections, which are
    // homogeneous). See token_surface.
    let detail = token_surface::parse_detail(args);
    let format = token_surface::parse_format(args);
    let render = |items: &[Value]| {
        token_surface::render_list(items, IMPACT_COLUMNS, "qualified_name", &detail, &format)
    };
    let callers_view = render(&callers.items);
    let importers_view = render(&importers.items);
    let users_view = render(&users.items);
    let implementors_view = render(&implementors.items);
    let references_view = render(&references.items);

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_impact",
        "qualified_name": qn,
        "communities": impact.communities,
        "communities_affected": impact.communities.len(),
        "processes": impact.processes,
        "processes_affected": impact.processes.len(),
        "detail": callers_view.detail,
        "format": callers_view.format,
        "callers": callers_view.value,
        "callers_total": callers.total_count,
        "offset": offset,
        "primary_list": "callers",
        "secondary_lists_paged": false,
        "importers": importers_view.value,
        "importers_total": importers.total_count,
        "users": users_view.value,
        "users_total": users.total_count,
        "implementors": implementors_view.value,
        "implementors_total": implementors.total_count,
        "references": references_view.value,
        "references_total": references.total_count,
        "dependents_total": dependents_total,
        // Code vs. reference fan-in split (issue #205): `code` is the same
        // count as `dependents_total` (callers+importers+users+implementors);
        // `references` is the doc/script cross-reference count. Reported
        // together so a caller doing "what's the real fan-in of this file?"
        // does not have to sum sections itself or mistake one dimension for
        // the whole.
        "counts": {
            "code": dependents_total,
            "references": references_total_full,
        },
        "truncated": any_truncated,
        // Epistemic honesty: is this blast radius exhaustive, or a lower bound?
        // `lower-bound` means real impact may exceed what is shown — because the
        // target is reached via dynamic dispatch and/or some edges were resolved
        // heuristically. `epistemic_reasons` names the carriers of uncertainty.
        // source: epistemic module; clustering::get_impact.
        "epistemic": impact.epistemic.as_str(),
        "epistemic_reasons": impact.epistemic_reasons,
    });
    if callers_view.columns.is_some() {
        // One header covers all four homogeneous sections.
        out["columns"] = json!(IMPACT_COLUMNS);
    }
    if let Some(next) = callers.next_offset {
        out["next_offset"] = json!(next);
    }

    // Issue #58: the symbol's FILE co-change partners are impact candidates the
    // static call graph cannot see (files that historically change together —
    // the architect agent's churning-pairs signal). Add them as a section under
    // the same detail/format surface. The symbol's file is the qn up to '::'.
    let file = qn.split("::").next().unwrap_or(qn);
    let partners = cochange_partners(&store, file);
    // Partners have their own shape ({file, cochange_count, coupling, jaccard}),
    // so render them with their own columns/id — NOT the impact `render` closure.
    let partner_view = token_surface::render_list(
        &partners,
        COCHANGE_PARTNER_COLUMNS,
        "file",
        &detail,
        &format,
    );
    out["cochange_partners"] = partner_view.value;
    out["cochange_partners_total"] = json!(partners.len());
    if partner_view.columns.is_some() {
        out["cochange_partner_columns"] = json!(COCHANGE_PARTNER_COLUMNS);
    }

    // Suggested follow-up traversals from this blast-radius result.
    out["next_steps"] = impact_next_steps(&impact, qn);

    // Cross-repo bridge: when sibling graphs are supplied, also surface callers
    // that live in OTHER repos. These are reported in their own section (not
    // merged into the local `callers`/`dependents_total`) so blast radius keeps
    // local and foreign impact distinct. Absent the arg this is a no-op.
    // source: cross-repo bridge spec (bridge module).
    let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
    if !siblings.is_empty() {
        let foreign = bridge::foreign_callers(&siblings, bridge::last_segment(qn));
        let handles: Vec<Value> = foreign.iter().map(|f| f.to_json()).collect();
        let foreign_page =
            response_budget::bound_values(handles, response_budget::per_section_chars());
        out["foreign_callers"] = json!(foreign_page.items);
        out["foreign_callers_total"] = json!(foreign.len());
        out["foreign_callers_paged"] = json!(false);
        if !siblings.skipped.is_empty() {
            out["sibling_graphs_skipped"] = json!(siblings.skipped);
        }
        // Cross-repo edges are name-matched without a shared linker, so any
        // foreign caller makes the blast radius a lower bound (and stays one
        // even if the local set was exact). source: epistemic module contract.
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
    Ok(out)
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
