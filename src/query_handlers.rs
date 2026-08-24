//! Stage 3a — `query_graph`: read-only Cypher execution, injection/safety
//! filters, and pagination/limit helpers. Extracted from `main.rs` per
//! issue #151 (Fowler: Extract Class).

use crate::token_surface;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use crate::graph_cache;
use crate::response_budget;

#[cfg(test)]
#[path = "query_handlers_tests.rs"]
mod tests;
use crate::indexing_handlers::*;

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

// ---------------------------------------------------------------------------
// Read-only Cypher filter — source: H3 fix.
//
// Rejects any query that contains a mutation or side-effectful keyword as a
// whole-word, case-insensitive match. This is a conservative allowlist-by-
// blocklist: the engine still validates syntax, we just refuse to hand it
// anything that could mutate state, load external data, or call procedures.
//
// COPY/EXPORT/IMPORT/ATTACH/LOAD (fleet-watch#15): these data-movement
// statements MUST be blocked here, because no engine-side gate catches them.
// lbug's `StatementReadWriteAnalyzer` classifies a statement as read-only from
// the compiled plan, and it overrides `visitCopyFrom` while leaving
// `visitCopyTo`, `visitExportDatabase`, `visitImportDatabase` and
// `visitAttachDatabase` at the base visitor's no-op — so all four come back
// read-only. Measured 2026-08-24 against lbug 0.19.1, on BOTH available engine
// gates: `PreparedStatement::is_read_only()` and a database opened with
// `SystemConfig::read_only(true)` (which reaches the same predicate via
// `ClientContext::validateTransaction`) each EXECUTE
// `COPY (..) TO 'f.csv'` and `EXPORT DATABASE 'd'`, writing the filesystem,
// while both refuse `CREATE NODE TABLE`. `LOAD FROM 'f'` likewise reads an
// arbitrary file. This lexical gate is therefore the only barrier against an
// attacker-named file write or read; the engine gate covers the disjoint
// family of database mutations reachable through syntax this scan misses.
// Pinned by `engine_gate_does_not_cover_filesystem_writes`.
//
// Identifier positions are exempt (see `contains_keyword_token`): `IMPORT` is
// also this schema's `Import` node table, so a keyword introduced by `:` or
// `.` is a label, relationship type, map key or property, not a clause.
// ---------------------------------------------------------------------------

pub(crate) const FORBIDDEN_CYPHER_KEYWORDS: &[&str] = &[
    "CREATE", "DELETE", "MERGE", "SET", "REMOVE", "DROP", "ALTER", "CALL", "LOAD", "COPY",
    "EXPORT", "IMPORT", "ATTACH",
];

/// Wall-clock bound (milliseconds) applied to a single `query_graph` execution
/// and then reset. Bounds a pathological read plan — e.g. an unbounded
/// variable-length `MATCH (a)-[*]->(b)` whose `LIMIT` caps rows, not traversal
/// work — so it cannot pin the single-threaded MCP worker. The MCP server is
/// mono-thread, so one unbounded query freezes every subsequent request; the
/// row-`LIMIT` and byte-page guards bound the RESULT, not the engine's
/// traversal effort.
///
/// source: provisional heuristic — a legitimate interactive graph query
/// returns in well under a second on the corpora this server indexes; 30_000 ms
/// is a generous ceiling only a pathological plan reaches. Operator-tunable if
/// a real workload is found to need longer (calibration: raise only against a
/// measured legitimate query that exceeds it).
pub(crate) const READ_QUERY_TIMEOUT_MS: u64 = 30_000;

/// Returns the first forbidden keyword found in `query`, or None if the query
/// is safe. Matching is whole-word, ASCII case-insensitive, and considers only
/// *executable* text: string literals, backtick-quoted identifiers and comments
/// are masked out first, and a keyword reached through `.` is a property name,
/// not a clause.
///
/// Why (issue #200): whole-word matching alone was not enough. A client that
/// looks a symbol up BY NAME — which is exactly what Cortex's process-symbols
/// path does during ingestion — sends `... WHERE s.name = 'load'`, and the
/// literal tripped the gate. `set`, `create`, `delete` and `call` are ordinary
/// function names in any codebase, so this rejected legitimate read queries
/// against any corpus containing them (observed 6x on automatised-pipeline and
/// 4x on cortex-viz in the 2026-08-06 A/B ingestion bench).
///
/// Masking is safe because nothing masked can execute: text inside a literal or
/// a comment is data, and `n.set` addresses a property. An UNTERMINATED literal
/// is the one case that could hide a real mutation from the scan, so it is
/// rejected outright (fail closed) rather than masked to end-of-input.
pub(crate) fn forbidden_cypher_keyword(query: &str) -> Option<&'static str> {
    let Some(executable) = mask_non_executable(query) else {
        // Unterminated literal/comment — refuse rather than scan a truncated
        // view of the query. Reported as its own sentinel so the caller still
        // gets an actionable read_only_query_required error.
        return Some("UNTERMINATED_LITERAL");
    };
    let upper = executable.to_ascii_uppercase();
    FORBIDDEN_CYPHER_KEYWORDS
        .iter()
        .find(|&&kw| contains_keyword_token(&upper, kw))
        .copied()
}

/// Replaces every non-executable region of `query` with spaces, preserving byte
/// length. Returns None when a literal or block comment is left unterminated.
///
/// Documented equivalent mutants (coding-standards.md §12.4). The scoped
/// cargo-mutants run over this function accounts for all 76 mutants: 56 caught,
/// 10 timed out, 10 survive. Every survivor is an off-by-one on one of the
/// bounds guards below (`<`→`<=`, `+`→`-`, `+`→`*`) or `&&`→`||` in the
/// comment-close check. They are unobservable: each only changes behaviour at a
/// position where the literal or comment is already unterminated, and every
/// such path converges on the same result (return None).
///
/// Established DIFFERENTIALLY, not by argument — reasoning got this wrong once
/// already. The original and each variant were run over EVERY string of length
/// 0..=7 drawn from the alphabet that drives these branches
/// (`'` `"` `` ` `` `\` `/` `*` `a` `\n`), 2,396,745 inputs each, comparing
/// outputs. The survivors listed above show 0 differing inputs. Three OTHER
/// mutants on the block-comment arm showed 10,751 / 7,326 / 2,394 differing
/// inputs — genuinely non-equivalent, and now killed by
/// `readonly_gate_block_comment_boundaries` and
/// `readonly_gate_handles_non_executable_regions_at_index_zero`. The
/// discriminating cases were `/**/`, `/*`, `/*/` and any region starting at
/// index 0, where `i - 1` underflows and `i * 1` coincides with `i`.
/// # source: measured 2026-08-07, differential harness /tmp/exh200b.rs.
///
/// The 10 timeouts stop the index advancing so the loop never terminates; a
/// hang is detection, not an escape.
pub(crate) fn mask_non_executable(query: &str) -> Option<String> {
    let b = query.as_bytes();
    let mut out = vec![b' '; b.len()];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1; // opening quote already blanked
                let mut closed = false;
                while i < b.len() {
                    // Backslash escapes the next byte inside quotes.
                    if b[i] == b'\\' && q != b'`' && i + 1 < b.len() {
                        i += 2;
                        continue;
                    }
                    if b[i] == q {
                        i += 1;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return None;
                }
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i += 2;
                let mut closed = false;
                while i + 1 < b.len() {
                    if b[i] == b'*' && b[i + 1] == b'/' {
                        i += 2;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return None;
                }
            }
            c => {
                out[i] = c;
                i += 1;
            }
        }
    }
    // Masked bytes are ASCII spaces and kept bytes came from a valid UTF-8
    // string at boundaries the ASCII-only matches never split.
    String::from_utf8(out).ok()
}

/// Whole-word match that additionally rejects *identifier* positions: a
/// keyword reached through `.` or `:` names a property, a node label, a
/// relationship type or a map key — never a clause. `n.set` is a property,
/// and `(i:Import)` is this schema's own `Import` node table.
pub(crate) fn contains_keyword_token(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let nbytes = needle.as_bytes();
    if nbytes.is_empty() || bytes.len() < nbytes.len() {
        return false;
    }
    let mut i = 0;
    while i + nbytes.len() <= bytes.len() {
        if &bytes[i..i + nbytes.len()] == nbytes {
            let left_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let right = i + nbytes.len();
            let right_ok = right == bytes.len() || !is_ident_char(bytes[right]);
            if left_ok && right_ok && !preceded_by_identifier_sigil(bytes, i) {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// True when `query` carries more than one statement, i.e. a `;` with
/// executable text after it. Semicolons inside string literals, backticked
/// identifiers and comments do not count — `mask_non_executable` blanks those
/// first, which is what makes this an exact split rather than a guess.
///
/// Why (fleet-watch review finding 9): `execute_read_only_query` must
/// `prepare` the statement to obtain the engine's read-only classification,
/// and `prepare` refuses a chain ("We do not support prepare multiple
/// statements", measured 2026-08-24 on lbug 0.19.1). Before the engine gate
/// existed the request went to `Connection::query`, which accepted a chain and
/// returned the LAST statement's rows — while this tool's LIMIT injection
/// appended to that last statement and its ORDER BY detection scanned the
/// whole chain. Rather than leak the engine's message, name the contract:
/// one statement per call. An unterminated literal is reported as a chain
/// here and refused by the keyword gate upstream, so it cannot slip through.
pub(crate) fn is_multi_statement(query: &str) -> bool {
    let Some(executable) = mask_non_executable(query) else {
        return true;
    };
    match executable.split_once(';') {
        Some((_, rest)) => !rest.trim().is_empty(),
        None => false,
    }
}

/// True when the token starting at `start` is introduced by `.` or `:`,
/// skipping intervening whitespace (`(i : Import)` is legal Cypher).
///
/// Why (fleet-watch review finding 1): the scan previously looked only at the
/// byte immediately left of the match and only for `.`, so `MATCH (i:Import)`
/// tripped the `IMPORT` entry and every query over this schema's `Import` node
/// table was refused — including the `MATCH (f:File)-[:Defines_File_Import]->
/// (n:Import)` shape the accuracy corpora in `benches/corpora/*/ground_truth
/// .json` are written against. Neither sigil can introduce a clause: after `:`
/// Cypher expects a label, a relationship type or a map key, and after `.` a
/// property name, so treating both as identifier positions loses no coverage
/// of the statements this gate exists to refuse.
fn preceded_by_identifier_sigil(bytes: &[u8], start: usize) -> bool {
    let mut j = start;
    while j > 0 && bytes[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    j > 0 && matches!(bytes[j - 1], b'.' | b':')
}

// ---------------------------------------------------------------------------
// Graph-path safety — source: H4 fix.
//
// The `graph_path` / `output_dir` / ... arguments are caller-controlled and
// in the pre-fix code were passed to `remove_dir_all`. A malicious caller
// could set `output_dir: "/"` and have the server wipe the filesystem.
//
// `validate_graph_path_safe` MUST be called before any `remove_dir_all` or
// `create_dir_all` on a caller-derived path. The policy:
//   (a) path must be absolute,
//   (b) last segment must be `graph` (or the path must contain `/graph/`),
//   (c) path must NOT equal a forbidden system root.
// ---------------------------------------------------------------------------

pub(crate) const FORBIDDEN_GRAPH_PATH_PREFIXES: &[&str] = &[
    "/", "/Users", "/home", "/root", "/tmp", "/var", "/etc", "/usr", "/bin", "/sbin", "/dev",
    "/opt", "/System", "/Library",
];

/// Returns Ok iff `path` is a safe target for destructive directory ops.
pub(crate) fn validate_graph_path_safe(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!(
            "unsafe_graph_path: must be absolute (got {:?})",
            path
        ));
    }
    // Must end in `/graph` (the well-known suffix). Check the last component.
    let last = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if last != "graph" {
        return Err(format!(
            "unsafe_graph_path: must end in '/graph' (got {:?})",
            path
        ));
    }
    // Reject pathological roots (even if they happen to end in `/graph`).
    let s = path.to_string_lossy();
    for forbidden in FORBIDDEN_GRAPH_PATH_PREFIXES {
        if s == *forbidden || s == format!("{forbidden}/graph") {
            return Err(format!(
                "unsafe_graph_path: {path:?} is a forbidden system path"
            ));
        }
    }
    Ok(())
}

/// Removes a stale graph artifact at `path`, whether the prior run left a
/// directory (older Kuzu lays the database out as a dir) or a single database
/// file (newer Kuzu). Plain `remove_dir_all` fails with `ENOTDIR (os error 20)`
/// when the target is a file — the observed failure on re-index of an existing
/// graph. `symlink_metadata` never traverses a symlink at the graph path, so a
/// symlinked `graph` is unlinked, not followed.
/// Caller MUST have run `validate_graph_path_safe` first.
pub(crate) fn remove_stale_graph_artifact(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("stat stale graph path: {e}"))?;
    let outcome = if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    outcome.map_err(|e| format!("remove stale graph artifact: {e}"))
}

/// Write a ``meta.json`` sidecar in ``output_dir`` recording the ABSOLUTE
/// source root this graph was indexed from.
///
/// AP stores file paths RELATIVE to the indexed root so the graph stays
/// portable across machines. A downstream consumer that must reconstruct
/// absolute paths — cortex-viz keys its FILE nodes by the absolute path (tool
/// events + wiki-page -> source-file joins) — needs that root, which is
/// otherwise consumed at index time and discarded. Persisting it in a sidecar
/// (not inside the graph) keeps the graph file itself free of machine-specific
/// paths: the structure stays portable, and the machine-specific root lives in
/// a file that is naturally regenerated on the next re-index.
///
/// Best-effort: a failed write is logged and ignored. The graph is the
/// product; the sidecar is a convenience for consumers, and its absence just
/// degrades a consumer's path reconstruction, never the index.
pub(crate) fn write_graph_meta(output_dir: &Path, root: &Path) {
    let meta = json!({
        "schema_version": 1,
        "root": root.to_string_lossy(),
        "tool": "ai-architect-mcp-codebase",
    });
    let meta_path = output_dir.join("meta.json");
    if let Err(e) = fs::write(&meta_path, meta.to_string()) {
        eprintln!("[ap] write graph meta {}: {e}", meta_path.display());
    }
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
