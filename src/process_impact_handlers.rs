//! Stage 3c — `get_processes` handler logic; `get_impact` lives in the
//! `impact` submodule and is re-exported here.
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use crate::token_surface;
use serde_json::{json, Value};
use std::path::Path;

use crate::clustering;
use crate::graph_cache;
use crate::response_budget;

mod impact;
pub(crate) use impact::*;

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
    let procs = sorted_process_rows(clustering::get_processes(&store)?);

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

/// Deterministic, page-safe process rows.
///
/// Cursor stability: get_processes runs `MATCH (p:Process) RETURN ...` with no
/// ORDER BY (clustering::process::get_processes), so the Ladybug scan order is
/// an engine implementation detail and is NOT guaranteed identical across
/// calls — paging over it would skip/duplicate rows. We impose a deterministic
/// total order here, at the read boundary, by (name, entry_point). Process
/// names are entry-point-derived and entry_point is a node id, so the pair is
/// unique, giving a total (tie-free) order. source: cursor-correctness
/// requirement (response_budget::BoundedPage docs).
fn sorted_process_rows(mut processes: Vec<clustering::ProcessInfo>) -> Vec<Value> {
    processes.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.entry_point.cmp(&b.entry_point))
    });
    processes
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
        .collect()
}

/// Scalar columns of a process row, in tabular-projection order (issue #56).
pub(crate) const PROCESS_COLUMNS: &[&str] =
    &["name", "entry_point", "entry_kind", "depth", "node_count"];
