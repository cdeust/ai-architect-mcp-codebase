//! `index_history` (temporal layer) plus Stage 3d `search_codebase` and
//! `get_context` handler logic. Extracted from `main.rs` per issue #151
//! (Fowler: Extract Class).

use crate::bridge;
use crate::epistemic;
use crate::token_surface;
use serde_json::{json, Value};
use std::path::Path;

use crate::graph_cache;
use crate::graph_store;
use crate::history;
use crate::response_budget;
use crate::search;

// ---------------------------------------------------------------------------
// History — index_history (temporal layer over the structural snapshot)
// ---------------------------------------------------------------------------

pub(crate) fn run_index_history(arguments: &Value) -> Value {
    match do_index_history(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "index_history_failed", "message": msg
        }),
    }
}

pub(crate) fn do_index_history(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let codebase_str = args
        .get("codebase_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'codebase_path'")?;
    let max_commits = args
        .get("max_commits")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let codebase_path = Path::new(codebase_str);
    if !codebase_path.exists() {
        return Err(format!("codebase_path does not exist: {codebase_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let result = history::index_history(&store, codebase_path, max_commits)?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_history",
        "commits": result.commits,
        "versions": result.versions,
        "commit_edges": result.commit_edges,
        "version_edges": result.version_edges,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3d — search_codebase
// ---------------------------------------------------------------------------

pub(crate) fn run_search_codebase(arguments: &Value) -> Value {
    match do_search_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "search_failed", "message": msg
        }),
    }
}

/// Scalar columns of a search hit, in tabular-projection order (issue #56). The
/// `processes` array field is intentionally omitted from the tabular projection
/// (a cell must be scalar); it stays available via the grouped `by_process`
/// section and in the default `detail:"full", format:"json"` objects.
pub(crate) const SEARCH_COLUMNS: &[&str] = &[
    "qualified_name",
    "name",
    "kind",
    "file_path",
    "score",
    "start_line",
    "end_line",
    "community_id",
];

pub(crate) fn do_search_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'query'")?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
    let label_filter = args
        .get("label_filter")
        .and_then(|v| v.as_str())
        .map(String::from);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // The search index lives in a sibling ``search_index/`` of the graph dir.
    // Pass it explicitly to search_graph — no process-global env hand-off
    // (that channel raced across parallel callers; see search::search_graph).
    // Shared with Stage 4's prepare_prd_input via search::resolve_search_index_dir
    // (issue #18) so both stages resolve the same graph to the same index.
    let search_index_dir = search::resolve_search_index_dir(graph_path);

    let start = std::time::Instant::now();
    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;
    let options = search::SearchOptions {
        limit,
        label_filter,
        min_score: 0.01,
    };
    let results = search::search_graph(&store, query, &options, search_index_dir.as_deref())?;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    // Cursor stability: search_graph returns results in a deterministic total
    // order — descending score with an ascending-qualified_name tie-break added
    // at every sort site (search::mod final sort, search::rrf::fuse). That total
    // order is identical across calls for a fixed graph + query, so paging by
    // `offset` over it neither skips nor duplicates rows. `limit` bounds the
    // ranked candidate universe; `offset` + byte budget page within it.
    // source: cursor-correctness requirement (response_budget::BoundedPage docs).
    let items: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "qualified_name": r.qualified_name,
                "name": r.name,
                "kind": r.label,
                "file_path": r.file_path,
                "score": format!("{:.4}", r.score),
                "community_id": r.community_id,
                "processes": r.process_names,
                "start_line": r.start_line,
                "end_line": r.end_line,
            })
        })
        .collect();

    // Page the ranked results by serialized size from `offset`. Previously this
    // tool relied solely on `limit`; a wide result set could still approach the
    // host cap and offered no way to retrieve rows beyond the cut. Byte-budget
    // paging makes the full ranked list retrievable in budget-sized pages.
    let page =
        response_budget::bound_values_paged(items, offset, response_budget::per_section_chars());

    // Process-grouped view: a lightweight secondary index over the returned
    // page. Built from `page.items` (not the full ranked set) so every
    // qualified_name it lists is present in `results` — the flat list stays the
    // single source of truth; `by_process` never duplicates row payload and
    // never references a row outside the page. source: search::group_hits_by_process.
    let group_input: Vec<(String, Vec<String>)> = page
        .items
        .iter()
        .map(|item| {
            let qn = item
                .get("qualified_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let processes = item
                .get("processes")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (qn, processes)
        })
        .collect();
    let by_process: Vec<Value> = search::group_hits_by_process(&group_input)
        .into_iter()
        .map(|(process, qualified_names)| {
            json!({
                "process": process,
                "qualified_names": qualified_names,
            })
        })
        .collect();

    // Token-surface shaping (issue #56): `detail:"ids"` returns bare qualified
    // names for a cheap wide sweep; `format:"tabular"` streams rows as arrays with
    // the columns declared once. Applied to the PAGED list, so the cursor contract
    // is untouched. `by_process` (built from the objects above) is unaffected.
    let detail = token_surface::parse_detail(args);
    let format = token_surface::parse_format(args);
    let view = token_surface::render_list(
        &page.items,
        SEARCH_COLUMNS,
        "qualified_name",
        &detail,
        &format,
    );

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "search_codebase",
        "query": query,
        "result_count": page.items.len(),
        "total_count": page.total_count,
        "offset": offset,
        "truncated": page.truncated,
        "detail": view.detail,
        "format": view.format,
        "results": view.value,
        "by_process": by_process,
        "elapsed_ms": elapsed_ms,
    });
    if let Some(cols) = view.columns {
        out["columns"] = cols;
    }
    if let Some(next) = page.next_offset {
        out["next_offset"] = json!(next);
    }

    // Cross-repo bridge: federate the query across sibling graphs. Foreign hits
    // are a bounded SECONDARY section (repo-tagged, not merged into the primary
    // cursored `results`) so the local `offset` contract stays exact. Absent the
    // arg this is a no-op. source: cross-repo bridge spec.
    let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
    if !siblings.is_empty() {
        let hits = bridge::federated_search(&siblings, query, limit);
        let foreign_items: Vec<Value> = hits.iter().map(|h| h.to_json()).collect();
        let foreign_page =
            response_budget::bound_values(foreign_items, response_budget::per_section_chars());
        out["foreign_results"] = json!(foreign_page.items);
        out["foreign_results_total"] = json!(hits.len());
        out["foreign_results_paged"] = json!(false);
        if !siblings.skipped.is_empty() {
            out["sibling_graphs_skipped"] = json!(siblings.skipped);
        }
    }

    // Suggest how to act on a hit: top-ranked results are the natural next
    // traversal anchors. Gated on a non-empty page so we never suggest acting
    // on nothing.
    if let Some(first) = page
        .items
        .first()
        .and_then(|r| r.get("qualified_name"))
        .and_then(|v| v.as_str())
    {
        out["next_steps"] = json!([
            format!("inspect the top hit: get_context on '{first}'"),
            "narrow further with `label_filter` (e.g. Function, Struct, Trait)".to_string(),
        ]);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Stage 3d — get_context
// ---------------------------------------------------------------------------

pub(crate) fn run_get_context(arguments: &Value) -> Value {
    match do_get_context(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "context_failed", "message": msg
        }),
    }
}

pub(crate) fn do_get_context(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'qualified_name'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;
    let ctx = match search::get_context(&store, qn) {
        Ok(c) => c,
        Err(search::GetContextError::NotFound(nf)) => {
            // source: C-correctness bug 2 — prefer a clean `symbol_not_found`
            // with did_you_mean over a cryptic string error. Return Ok(Value)
            // because the outer `run_get_context` would otherwise wrap this
            // under `context_failed`.
            return Ok(json!({
                "stage": 3,
                "status": "error",
                "reason": "symbol_not_found",
                "message": format!("not found: {}", nf.input),
                "did_you_mean": nf.did_you_mean,
            }));
        }
        Err(search::GetContextError::Other(m)) => return Err(m),
    };

    let related_to_json = |items: &[search::RelatedSymbol]| -> Vec<Value> {
        items
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "qualified_name": s.qualified_name,
                    "kind": s.label,
                })
            })
            .collect()
    };

    let community_json = ctx.community.as_ref().map(|c| {
        json!({
            "id": c.id,
            "name": c.name,
            "member_count": c.member_count,
        })
    });

    let processes_json: Vec<Value> = ctx
        .processes
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "role": p.role,
            })
        })
        .collect();

    // Epistemic honesty: when the symbol is a dynamic-dispatch surface (a
    // trait/interface), its reverse relationships (`called_by`, `used_by`) are a
    // LOWER BOUND — polymorphic call sites that go through the interface are not
    // exhaustively attributable to it by static resolution. Concrete symbols are
    // exact. source: epistemic module.
    let dynamic_surface = epistemic::is_dynamic_dispatch_surface(&ctx.label);
    let (epistemic_status, epistemic_reasons) = if dynamic_surface {
        (
            epistemic::Boundary::LowerBound.as_str(),
            vec![format!(
                "'{}' is a {} (dynamic-dispatch surface): `called_by`/`used_by` \
                 omit call sites that reach it polymorphically; treat the reverse \
                 relationships as a lower bound and consult `implemented_by`",
                ctx.qualified_name, ctx.label
            )],
        )
    } else {
        (epistemic::Boundary::Exact.as_str(), Vec::new())
    };

    let mut next_steps = vec![format!(
        "trace blast radius: get_impact on '{}'",
        ctx.qualified_name
    )];
    if dynamic_surface && !ctx.implemented_by.is_empty() {
        next_steps.push(
            "enumerate concrete behaviour: get_symbol on an `implemented_by[].qualified_name`"
                .to_string(),
        );
    }

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_context",
        "symbol": {
            "qualified_name": ctx.qualified_name,
            "name": ctx.name,
            "kind": ctx.label,
            "file_path": ctx.file_path,
            "start_line": ctx.start_line,
            "end_line": ctx.end_line,
            "visibility": ctx.visibility,
        },
        "relationships": {
            "imports": related_to_json(&ctx.imports),
            "imported_by": related_to_json(&ctx.imported_by),
            "calls": related_to_json(&ctx.calls),
            "called_by": related_to_json(&ctx.called_by),
            "implements": related_to_json(&ctx.implements),
            "implemented_by": related_to_json(&ctx.implemented_by),
            "uses": related_to_json(&ctx.uses),
            "used_by": related_to_json(&ctx.used_by),
        },
        "community": community_json,
        "processes": processes_json,
        "epistemic": epistemic_status,
        "epistemic_reasons": epistemic_reasons,
        "next_steps": next_steps,
    }))
}
