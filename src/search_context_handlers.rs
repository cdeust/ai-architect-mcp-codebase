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
    let mut out = match do_search_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "search_failed", "message": msg
        }),
    };
    // fleet-watch#112: the tool's single exit, so the freshness receipt reaches
    // every answer — including a store-open or query failure, which is what an
    // in-progress re-index looks like from here.
    crate::graph_freshness::attach_from_arguments(&mut out, arguments);
    out
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

/// The caller-supplied half of a `search_codebase` call, validated once.
///
/// A parameter object rather than six positional arguments (§4.4): the query
/// text, its ranking bounds and its cursor travel together through every helper
/// below, and splitting them would put the cursor's `offset` and the ranking's
/// `limit` in different places.
struct SearchRequest<'a> {
    graph_str: &'a str,
    query: &'a str,
    limit: usize,
    offset: u64,
    label_filter: Option<String>,
}

impl<'a> SearchRequest<'a> {
    /// Defaults: `limit` 20 ranked candidates, `offset` 0 (first page), no
    /// label filter. source: the tool's own JSON schema in `tool_schemas`.
    fn parse(args: &'a serde_json::Map<String, Value>) -> Result<Self, String> {
        Ok(SearchRequest {
            graph_str: args
                .get("graph_path")
                .and_then(|v| v.as_str())
                .ok_or("missing required field 'graph_path'")?,
            query: args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or("missing required field 'query'")?,
            limit: args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize,
            offset: args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0),
            label_filter: args
                .get("label_filter")
                .and_then(|v| v.as_str())
                .map(String::from),
        })
    }
}

/// One JSON row per ranked hit, before paging and before token-surface shaping.
fn search_hit_rows(results: &[search::SearchResult]) -> Vec<Value> {
    results
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
        .collect()
}

/// Pages the ranked hits by serialized size, starting at `offset`.
///
/// Cursor stability: search_graph returns results in a deterministic total
/// order — descending score with an ascending-qualified_name tie-break added
/// at every sort site (search::mod final sort, search::rrf::fuse). That total
/// order is identical across calls for a fixed graph + query, so paging by
/// `offset` over it neither skips nor duplicates rows. `limit` bounds the
/// ranked candidate universe; `offset` + byte budget page within it.
/// source: cursor-correctness requirement (response_budget::BoundedPage docs).
///
/// Previously this tool relied solely on `limit`; a wide result set could still
/// approach the host cap and offered no way to retrieve rows beyond the cut.
/// Byte-budget paging makes the full ranked list retrievable in budget-sized
/// pages.
fn page_search_results(
    results: &[search::SearchResult],
    offset: u64,
) -> response_budget::BoundedPage {
    response_budget::bound_values_paged(
        search_hit_rows(results),
        offset,
        response_budget::per_section_chars(),
    )
}

/// Token-surface shaping (issue #56): `detail:"ids"` returns bare qualified
/// names for a cheap wide sweep; `format:"tabular"` streams rows as arrays with
/// the columns declared once. Applied to the PAGED list, so the cursor contract
/// is untouched. `by_process` (built from the same objects) is unaffected.
fn shape_search_page(
    page_items: &[Value],
    args: &serde_json::Map<String, Value>,
) -> token_surface::ListView {
    token_surface::render_list(
        page_items,
        SEARCH_COLUMNS,
        "qualified_name",
        &token_surface::parse_detail(args),
        &token_surface::parse_format(args),
    )
}

/// Process-grouped view: a lightweight secondary index over the returned
/// page. Built from `page_items` (not the full ranked set) so every
/// qualified_name it lists is present in `results` — the flat list stays the
/// single source of truth; `by_process` never duplicates row payload and
/// never references a row outside the page. source: search::group_hits_by_process.
fn search_process_groups(page_items: &[Value]) -> Vec<Value> {
    let group_input: Vec<(String, Vec<String>)> = page_items
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
    search::group_hits_by_process(&group_input)
        .into_iter()
        .map(|(process, qualified_names)| {
            json!({
                "process": process,
                "qualified_names": qualified_names,
            })
        })
        .collect()
}

/// Cross-repo bridge: federate the query across sibling graphs. Foreign hits
/// are a bounded SECONDARY section (repo-tagged, not merged into the primary
/// cursored `results`) so the local `offset` contract stays exact. Absent the
/// arg this is a no-op. source: cross-repo bridge spec.
fn attach_foreign_results(
    out: &mut Value,
    arguments: &Value,
    graph_path: &Path,
    req: &SearchRequest<'_>,
) {
    let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
    if siblings.is_empty() {
        return;
    }
    let hits = bridge::federated_search(&siblings, req.query, req.limit);
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

/// Suggest how to act on a hit: top-ranked results are the natural next
/// traversal anchors. Gated on a non-empty page so we never suggest acting
/// on nothing.
fn attach_search_next_steps(out: &mut Value, page_items: &[Value]) {
    if let Some(first) = page_items
        .first()
        .and_then(|r| r.get("qualified_name"))
        .and_then(|v| v.as_str())
    {
        out["next_steps"] = json!([
            format!("inspect the top hit: get_context on '{first}'"),
            "narrow further with `label_filter` (e.g. Function, Struct, Trait)".to_string(),
        ]);
    }
}

/// The `search_codebase` response envelope, before the optional sections.
fn search_envelope(
    req: &SearchRequest<'_>,
    page: &response_budget::BoundedPage,
    view: &token_surface::ListView,
    by_process: Vec<Value>,
    elapsed_ms: u64,
) -> Value {
    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "search_codebase",
        "query": req.query,
        "result_count": page.items.len(),
        "total_count": page.total_count,
        "offset": req.offset,
        "truncated": page.truncated,
        "detail": view.detail,
        "format": view.format,
        "results": view.value,
        "by_process": by_process,
        "elapsed_ms": elapsed_ms,
    });
    if let Some(cols) = view.columns.clone() {
        out["columns"] = cols;
    }
    if let Some(next) = page.next_offset {
        out["next_offset"] = json!(next);
    }
    out
}

pub(crate) fn do_search_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let req = SearchRequest::parse(args)?;

    let graph_path = Path::new(req.graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {}", req.graph_str));
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
        limit: req.limit,
        label_filter: req.label_filter.clone(),
        min_score: 0.01,
    };
    let results = search::search_graph(&store, req.query, &options, search_index_dir.as_deref())?;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    let page = page_search_results(&results, req.offset);
    let by_process = search_process_groups(&page.items);
    let view = shape_search_page(&page.items, args);

    let mut out = search_envelope(&req, &page, &view, by_process, elapsed_ms);
    attach_foreign_results(&mut out, arguments, graph_path, &req);
    attach_search_next_steps(&mut out, &page.items);
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

    Ok(context_envelope(&ctx))
}

/// One JSON object per related symbol, in the shape every relationship list of
/// `get_context` uses.
fn related_symbols_json(items: &[search::RelatedSymbol]) -> Vec<Value> {
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
}

/// Epistemic honesty: when the symbol is a dynamic-dispatch surface (a
/// trait/interface), its reverse relationships (`called_by`, `used_by`) are a
/// LOWER BOUND — polymorphic call sites that go through the interface are not
/// exhaustively attributable to it by static resolution. Concrete symbols are
/// exact. source: epistemic module.
fn context_epistemic(ctx: &search::SymbolContext) -> (&'static str, Vec<String>) {
    if !epistemic::is_dynamic_dispatch_surface(&ctx.label) {
        return (epistemic::Boundary::Exact.as_str(), Vec::new());
    }
    (
        epistemic::Boundary::LowerBound.as_str(),
        vec![format!(
            "'{}' is a {} (dynamic-dispatch surface): `called_by`/`used_by` \
             omit call sites that reach it polymorphically; treat the reverse \
             relationships as a lower bound and consult `implemented_by`",
            ctx.qualified_name, ctx.label
        )],
    )
}

/// Suggested follow-up traversals. The second step is offered only when there
/// is something concrete to enumerate behind the dispatch surface.
fn context_next_steps(ctx: &search::SymbolContext) -> Vec<String> {
    let mut steps = vec![format!(
        "trace blast radius: get_impact on '{}'",
        ctx.qualified_name
    )];
    if epistemic::is_dynamic_dispatch_surface(&ctx.label) && !ctx.implemented_by.is_empty() {
        steps.push(
            "enumerate concrete behaviour: get_symbol on an `implemented_by[].qualified_name`"
                .to_string(),
        );
    }
    steps
}

/// The `get_context` response envelope for a resolved symbol.
fn context_envelope(ctx: &search::SymbolContext) -> Value {
    let (epistemic_status, epistemic_reasons) = context_epistemic(ctx);
    json!({
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
            "imports": related_symbols_json(&ctx.imports),
            "imported_by": related_symbols_json(&ctx.imported_by),
            "calls": related_symbols_json(&ctx.calls),
            "called_by": related_symbols_json(&ctx.called_by),
            "implements": related_symbols_json(&ctx.implements),
            "implemented_by": related_symbols_json(&ctx.implemented_by),
            "uses": related_symbols_json(&ctx.uses),
            "used_by": related_symbols_json(&ctx.used_by),
        },
        "community": ctx.community.as_ref().map(|c| json!({
            "id": c.id,
            "name": c.name,
            "member_count": c.member_count,
        })),
        "processes": ctx.processes.iter().map(|p| json!({
            "name": p.name,
            "role": p.role,
        })).collect::<Vec<_>>(),
        "epistemic": epistemic_status,
        "epistemic_reasons": epistemic_reasons,
        "next_steps": context_next_steps(ctx),
    })
}

#[cfg(test)]
#[path = "search_context_handlers_tests.rs"]
mod tests;
