//! Stage 3a — `query_graph`: read-only Cypher execution, injection/safety
//! filters, and pagination/limit helpers. Extracted from `main.rs` per
//! issue #151 (Fowler: Extract Class).

use crate::token_surface;
use serde_json::{json, Value};
use std::path::Path;

use crate::graph_cache;
use crate::response_budget;

#[cfg(test)]
#[path = "query_handlers_tests.rs"]
mod tests;
use crate::indexing_handlers::*;

mod graph_paths;
mod read_only_gate;

pub(crate) use graph_paths::{
    remove_stale_graph_artifact, validate_graph_path_safe, write_graph_meta,
};
use read_only_gate::{forbidden_cypher_keyword, is_multi_statement, READ_QUERY_TIMEOUT_MS};

// ---------------------------------------------------------------------------
// Stage 3a — query_graph
// ---------------------------------------------------------------------------

pub(crate) fn run_query_graph(arguments: &Value) -> Value {
    match do_query_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3,
            "status": "error",
            "reason": query_failure_reason(&msg),
            "message": msg,
        }),
    }
}

/// Maps a `do_query_graph` error onto a documented reason code, so a caller
/// can tell a policy rejection from an engine error without parsing prose.
/// Anything unrecognised stays `query_failed`.
fn query_failure_reason(msg: &str) -> &'static str {
    for reason in ["read_only_query_required", "multi_statement_not_supported"] {
        if msg.contains(reason) {
            return reason;
        }
    }
    "query_failed"
}

pub(crate) fn do_query_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = Path::new(graph_str);

    // Issue #57 — `graph: "missed"` enumerates what the index does NOT cover
    // (parse-incomplete + skipped + quarantined files) instead of running Cypher,
    // so an agent doing graph queries can pivot to grep for the gaps. `query` is
    // not required in this mode. source: DeusData/codebase-memory-mcp
    // query_graph(graph="missed").
    let graph_mode = args
        .get("graph")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    if graph_mode == "missed" {
        if !graph_path.exists() {
            return Err(format!("graph_path does not exist: {graph_str}"));
        }
        return Ok(query_missed_response(graph_path));
    }

    let (query, offset) = gated_cypher_request(args, graph_path, graph_str)?;
    let mut out = run_gated_cypher(graph_path, query, offset)?;
    shape_token_surface(args, &mut out);
    Ok(out)
}

/// Executes an already-gated single Cypher statement and pages the result.
/// Split from `do_query_graph` so the request-shaping steps (mode routing,
/// gating, token-surface shaping) stay separate from execution (§4.2).
fn run_gated_cypher(graph_path: &Path, query: &str, offset: u64) -> Result<Value, String> {
    // Bound caller-supplied Cypher: if it has no LIMIT clause, inject one so an
    // unbounded MATCH cannot return enough rows to blow the host's MCP
    // tool-result cap. Queries that already declare a LIMIT are left untouched.
    let (effective_query, limit_injected) = inject_limit_if_absent(query);

    let start = std::time::Instant::now();
    // Read-only tool: reuse the process-local cached handle instead of
    // re-opening the embedded DB per request. Cache revalidates on-disk
    // staleness on every call. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;
    // Engine-authoritative database-write guard + wall-clock bound — the layer
    // behind the lexical pre-filter above, covering a disjoint family of
    // statements; the division of labor is documented on
    // FORBIDDEN_CYPHER_KEYWORDS. source: fleet-watch#15.
    let qr = store.execute_read_only_query(&effective_query, READ_QUERY_TIMEOUT_MS)?;

    Ok(paged_query_response(
        &qr,
        QueryPageMeta {
            offset,
            elapsed_ms: start.elapsed().as_millis() as u64,
            // Cursor stability for query_graph is the caller's responsibility,
            // and we report whether it holds rather than silently shipping an
            // unsafe cursor — see paged_query_response.
            order_stable: has_order_by_clause(query),
            limit_injected,
        },
    ))
}

/// Extracts and gates the Cypher request: `query` is required and must pass
/// the lexical read-only filter BEFORE any filesystem check, so a mutation
/// attempt is refused even for a nonexistent path (source: H3 fix — security
/// ordering); the graph must then exist on disk. Returns `(query, offset)`.
fn gated_cypher_request<'a>(
    args: &'a serde_json::Map<String, Value>,
    graph_path: &Path,
    graph_str: &str,
) -> Result<(&'a str, u64), String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'query'")?;
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
    if let Some(bad) = forbidden_cypher_keyword(query) {
        return Err(format!(
            "read_only_query_required: query_graph is read-only; \
             found forbidden keyword: {bad}"
        ));
    }
    if is_multi_statement(query) {
        return Err(
            "multi_statement_not_supported: query_graph executes exactly one \
             statement per call. The engine's read-only classification and this \
             tool's LIMIT injection, ORDER BY detection and offset cursor are \
             all properties of a single statement, so a `;`-chained request has \
             no well-defined page. Send one statement per call; a trailing `;` \
             is accepted."
                .to_string(),
        );
    }
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    Ok((query, offset))
}

/// Response-envelope metadata for one `query_graph` page, grouped so
/// `paged_query_response` takes one parameter object (§4.4).
struct QueryPageMeta {
    offset: u64,
    elapsed_ms: u64,
    order_stable: bool,
    limit_injected: bool,
}

/// Builds the paged `query_graph` response envelope.
///
/// Row order is whatever the caller's Cypher produces. Ladybug (like Cypher
/// generally) does NOT guarantee a stable row order WITHOUT an `ORDER BY`
/// clause — the scan order is an engine implementation detail. We cannot
/// inject an `ORDER BY` into arbitrary Cypher safely (RETURN items may be
/// aggregates/expressions with no addressable sort key) the way we inject
/// `LIMIT`, so the caller detects whether one was declared and passes
/// `order_stable` so a client knows whether paging over `offset` is safe.
/// source: Cypher/Kuzu ordering semantics — without ORDER BY, result order is
/// unspecified; see response_budget::BoundedPage docs for why an unstable
/// order makes a cursor skip/duplicate rows.
///
/// Second-stage guard: even with the row LIMIT, wide rows (e.g. `RETURN n`
/// serializing whole nodes) can exceed the byte budget. Page by serialized
/// size from `offset` so the caller can pace through a large result set; the
/// page is cursor-safe only when `order_stable` is true.
fn paged_query_response(qr: &crate::graph_store::QueryResult, meta: QueryPageMeta) -> Value {
    let all_rows: Vec<Value> = qr
        .rows
        .iter()
        .map(|row| Value::Array(row.iter().map(|c| json!(c)).collect()))
        .collect();
    let page = response_budget::bound_values_paged(
        all_rows,
        meta.offset,
        response_budget::MAX_RESPONSE_CHARS,
    );
    let returned_rows = &page.items;

    // Rebuild the human-readable string from only the rows on THIS page (the
    // window [offset, offset + returned_count)) so it stays within budget
    // alongside the structured `rows`.
    let start = (meta.offset as usize).min(qr.rows.len());
    let returned_string_rows: Vec<Vec<String>> = qr
        .rows
        .iter()
        .skip(start)
        .take(returned_rows.len())
        .cloned()
        .collect();

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "query_graph",
        "columns": qr.columns,
        "rows": returned_rows,
        "result": format_query_result(&qr.columns, &returned_string_rows),
        "elapsed_ms": meta.elapsed_ms,
        "total_count": page.total_count,
        "returned_count": returned_rows.len(),
        "offset": meta.offset,
        "truncated": page.truncated,
        "order_stable": meta.order_stable,
        "limit_injected": meta.limit_injected,
    });
    if let Some(next) = page.next_offset {
        out["next_offset"] = json!(next);
    }
    out
}

/// Token-surface shaping (issue #56). query_graph already emits `rows` as
/// compact arrays with `columns` declared once — the native tabular shape. The
/// token hog is the human-readable `result` string that duplicates the rows,
/// so the compact modes drop it:
///   * detail:"ids" → collapse to the FIRST column's values (a bare id
///     list) and drop `result` — the cheap "which nodes match" sweep.
///   * format:"tabular" → keep columns+rows, drop `result`.
///   * default (full/json) → unchanged (columns + rows + result string).
fn shape_token_surface(args: &serde_json::Map<String, Value>, out: &mut Value) {
    match token_surface::parse_detail(args) {
        token_surface::Detail::Ids => {
            let ids: Vec<Value> = out["rows"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|r| r.as_array().and_then(|cells| cells.first()).cloned())
                        .collect()
                })
                .unwrap_or_default();
            let first_col: Vec<Value> = out["columns"]
                .as_array()
                .and_then(|c| c.first())
                .cloned()
                .into_iter()
                .collect();
            if let Some(obj) = out.as_object_mut() {
                obj.insert("rows".into(), json!(ids));
                obj.insert("columns".into(), json!(first_col));
                obj.remove("result");
                obj.insert("detail".into(), json!("ids"));
                obj.insert("format".into(), json!("ids"));
            }
        }
        token_surface::Detail::Full => match token_surface::parse_format(args) {
            token_surface::Format::Tabular => {
                if let Some(obj) = out.as_object_mut() {
                    obj.remove("result");
                    obj.insert("detail".into(), json!("full"));
                    obj.insert("format".into(), json!("tabular"));
                }
            }
            token_surface::Format::Json => {
                out["detail"] = json!("full");
                out["format"] = json!("json");
            }
        },
    }
}

/// Maximum rows injected into a caller's Cypher when it declares no LIMIT.
///
/// source: derived from the response budget — `MAX_RESPONSE_CHARS / typical
/// row chars`. A typical structured row in this graph serializes to ~90–140
/// chars (measured in `response_budget::tests::measure_representative_row_size`);
/// `100_000 / 140 ≈ 714`. We round down to a conservative bound so the injected
/// LIMIT alone keeps even moderately wide rows under the cap, with the
/// byte-budget pass above as the exact backstop.
pub(crate) const QUERY_GRAPH_ROW_LIMIT: usize = 500;

/// Appends `LIMIT <QUERY_GRAPH_ROW_LIMIT>` to `query` when no LIMIT clause is
/// already present. Returns the (possibly rewritten) query and whether a limit
/// was injected.
///
/// precondition: `query` has already passed `forbidden_cypher_keyword` (it is a
/// read-only query).
/// postcondition: the returned query contains a LIMIT clause; if the input
/// already had one the input is returned verbatim (`limit_injected == false`).
pub(crate) fn inject_limit_if_absent(query: &str) -> (String, bool) {
    if has_limit_clause(query) {
        return (query.to_string(), false);
    }
    // Strip a trailing semicolon/whitespace before appending so we don't emit
    // `... ; LIMIT n`, which Cypher rejects.
    let trimmed = query.trim_end().trim_end_matches(';').trim_end();
    (format!("{trimmed} LIMIT {QUERY_GRAPH_ROW_LIMIT}"), true)
}

/// Detects whether a Cypher query already declares a `LIMIT` clause.
///
/// Matches the keyword `LIMIT` (case-insensitive) only when it stands as a word
/// — not as a substring of an identifier like `node_limit`. This is a syntactic
/// guard, not a full parser: the graph engine itself rejects malformed Cypher.
pub(crate) fn has_limit_clause(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find("limit") {
        let start = idx + pos;
        let end = start + "limit".len();
        let prev_ok = start == 0 || !is_ident_char(bytes[start - 1]);
        let next_ok = end >= bytes.len() || !is_ident_char(bytes[end]);
        if prev_ok && next_ok {
            return true;
        }
        idx = end;
    }
    false
}

pub(crate) fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Detects whether a Cypher query declares an `ORDER BY` clause.
///
/// Used to report `order_stable` on `query_graph` responses: an `offset` cursor
/// over the rows is only safe when the caller pinned a deterministic order with
/// `ORDER BY` (without it, the engine's row order is unspecified). Matches the
/// two keywords `order` and `by` as whole words separated only by whitespace,
/// case-insensitively. This is a syntactic guard, not a full parser — it can be
/// fooled by `ORDER BY` inside a string literal, which is acceptable for an
/// advisory flag (the engine remains the source of truth for the query plan).
pub(crate) fn has_order_by_clause(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find("order") {
        let start = idx + pos;
        let end = start + "order".len();
        let prev_ok = start == 0 || !is_ident_char(bytes[start - 1]);
        if prev_ok {
            // Skip whitespace after "order", then require the word "by".
            let mut j = end;
            let had_ws = j < bytes.len() && bytes[j].is_ascii_whitespace();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if had_ws && lower[j..].starts_with("by") {
                let by_end = j + "by".len();
                let next_ok = by_end >= bytes.len() || !is_ident_char(bytes[by_end]);
                if next_ok {
                    return true;
                }
            }
        }
        idx = end;
    }
    false
}

pub(crate) fn format_query_result(columns: &[String], rows: &[Vec<String>]) -> String {
    let mut out = columns.join(" | ");
    for row in rows {
        out.push('\n');
        out.push_str(&row.join(" | "));
    }
    out
}
