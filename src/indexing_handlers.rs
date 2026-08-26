//! Stage 3a — `index_codebase` / `index_status` / `ingest_traces` handler
//! logic. Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use crate::artifact;
use crate::cochange;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use crate::handler_util::*;
use crate::history_handlers::*;
use crate::indexer;
use crate::query_handlers::*;

// ---------------------------------------------------------------------------
// Stage 3a — index_codebase
// ---------------------------------------------------------------------------

pub(crate) fn run_index_codebase(arguments: &Value) -> Value {
    match do_index_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "index_failed", "message": msg
        }),
    }
}

/// Everything `index_codebase` needs after its arguments have been validated
/// and the output directory prepared.
///
/// A parameter object rather than ten positional arguments (§4.4): these values
/// are derived together, travel together through bootstrap / incremental / full
/// paths, and are meaningless apart.
struct IndexRequest {
    codebase: std::path::PathBuf,
    output_dir: std::path::PathBuf,
    graph_dir: std::path::PathBuf,
    manifest_path: std::path::PathBuf,
    options: indexer::IndexOptions,
    want_export: bool,
    want_bootstrap: bool,
    accept_stale: bool,
    want_full: bool,
    want_cochange: bool,
}

impl IndexRequest {
    /// Validates the caller's arguments and prepares the output directory.
    ///
    /// Touches the filesystem deliberately — hence `prepare`, not `parse`: the
    /// legacy artifact-dir migration (#195) must run before anything walks the
    /// tree, and `validate_graph_path_safe` must run before any destructive op
    /// on the derived `graph/` path (source: H4 fix).
    fn prepare(args: &serde_json::Map<String, Value>) -> Result<Self, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'path'")?;
        let output_str = args
            .get("output_dir")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'output_dir'")?;
        let options = indexer::IndexOptions {
            language_filter: parse_language_filter(args)?,
            dependency_scope: parse_dependency_scope(args)?,
            exclude_dirs: parse_exclude_dirs(args)?,
        };
        let codebase = require_absolute(path_str, "path")?;
        if !codebase.exists() {
            return Err(format!("path does not exist: {}", codebase.display()));
        }
        artifact::migrate_legacy_dir(&codebase);
        let output_dir = require_absolute(output_str, "output_dir")?;
        fs::create_dir_all(&output_dir).map_err(|e| format!("create output dir: {e}"))?;
        let graph_dir = output_dir.join("graph");
        validate_graph_path_safe(&graph_dir)?;
        let manifest_path = indexer::manifest::manifest_path(&output_dir);
        Ok(IndexRequest {
            codebase,
            output_dir,
            graph_dir,
            manifest_path,
            options,
            want_export: parse_bool_arg(args, "export_artifact", false)?,
            want_bootstrap: parse_bool_arg(args, "bootstrap", false)?,
            accept_stale: parse_bool_arg(args, "accept_stale", false)?,
            // Issue #62: force a from-scratch rebuild even when an incremental
            // baseline exists (e.g. after changing the language filter or
            // dependency scope, which the manifest does not capture).
            want_full: parse_bool_arg(args, "full", false)?,
            // Issue #58: mine git temporal coupling (FILE_CHANGES_WITH) after
            // indexing. Cheap (one bounded `git log`) and the architect agent
            // reads churning pairs straight from the graph.
            want_cochange: parse_bool_arg(args, "cochange", true)?,
        })
    }
}

/// Issue #62 incremental mode: when a prior local graph AND a readable file
/// manifest exist and the caller did not force `full`, re-index only the changed
/// files. `None` means "not applicable, or it failed" — a failure logs loudly
/// and the caller falls through to a full rebuild (§13 — no silent path).
fn try_incremental_index(req: &IndexRequest) -> Option<Value> {
    if req.want_full || !req.graph_dir.exists() {
        return None;
    }
    let prior = indexer::manifest::load(&req.manifest_path)?;
    match indexer::index_incremental(
        &req.codebase,
        &req.graph_dir,
        &req.manifest_path,
        &req.options,
        &prior,
    ) {
        Ok(inc) => {
            // `index_incremental` has already saved the refreshed manifest, so
            // `meta.json` lands last and names it (see write_graph_meta).
            let meta_err = write_graph_meta(&req.output_dir, &req.codebase).err();
            let mut resp = finish_incremental_response(
                inc,
                &req.graph_dir,
                &req.codebase,
                &req.manifest_path,
                req.want_export,
                req.want_cochange,
            );
            report_meta_error(&mut resp, meta_err);
            Some(resp)
        }
        Err(e) => {
            eprintln!("[ap] incremental index failed ({e}); falling back to a full re-index");
            None
        }
    }
}

/// Writes the three sidecars a completed full index leaves beside the graph, in
/// the one order every path uses, and mines temporal coupling.
///
/// ORDER IS LOAD-BEARING (fleet-watch#112 review round 4): the manifest is
/// written BEFORE `meta.json`, matching the incremental path, so `meta.json` is
/// the single commit point of an index and can record which manifest it
/// accompanies. Written the other way round — as this path did — a query-time
/// reader landing between the two got a fresh commit sha paired with the
/// previous manifest and called a just-rebuilt graph stale, in a plain
/// single-process run with no concurrent writer.
///
/// All three writes are best-effort: a failed sidecar costs the next run its
/// incremental baseline or an honesty signal, never this index.
/// Surfaces a failed `meta.json` write on the response instead of leaving the
/// caller believing a complete index landed.
///
/// The index itself DID succeed — the graph is the product — so this is a note
/// on an `ok` response rather than a failure. But the sidecar is what the
/// query-time staleness receipt reads, so a caller that does not learn it is
/// missing would go on trusting a receipt that has nothing to stand on
/// (fleet-watch#112 review round 5).
fn report_meta_error(response: &mut Value, err: Option<String>) {
    let Some(err) = err else { return };
    eprintln!("[ap] graph meta sidecar write failed (index succeeded): {err}");
    response["meta_write_error"] = json!(err);
}

fn persist_full_index_sidecars(
    req: &IndexRequest,
    result: &indexer::IndexResult,
) -> (std::path::PathBuf, Value, Option<String>) {
    if let Err(e) = indexer::write_full_manifest(&req.codebase, &req.manifest_path, &req.options) {
        eprintln!("[ap] file manifest write failed (index succeeded): {e}");
    }
    // Records the absolute source root beside the graph; relative file paths
    // stay in the graph, and the root lets consumers rebuild absolute ones.
    let meta_err = write_graph_meta(&req.output_dir, &req.codebase).err();
    // Issue #57: the coverage-honesty sidecar (parse-incomplete, skipped, and
    // quarantined files).
    let coverage_path = indexer::coverage::coverage_path(&req.output_dir);
    if let Err(e) = indexer::coverage::save(&coverage_path, &result.coverage) {
        eprintln!("[ap] coverage sidecar write failed (index succeeded): {e}");
    }
    // Issue #58: full index → full re-mine of the co-change window.
    let cochange_summary = if req.want_cochange {
        run_cochange(
            &req.graph_dir,
            &req.codebase,
            &req.output_dir,
            cochange::Mode::Full,
        )
    } else {
        Value::Null
    };
    (coverage_path, cochange_summary, meta_err)
}

/// Issue #55: an explicit index is the single (best-ratio, zstd-9) export tier.
/// Failure to export is LOUD but non-fatal — the index itself succeeded.
fn attach_artifact_export(
    response: &mut Value,
    req: &IndexRequest,
    result: &indexer::IndexResult,
    coverage_path: &Path,
) {
    match artifact::export_artifact(
        &req.graph_dir,
        &req.codebase,
        result.node_count,
        result.edge_count,
        Some(&req.manifest_path),
        Some(coverage_path),
    ) {
        Ok(stats) => {
            response["artifact_path"] = json!(stats.artifact_path.to_string_lossy());
            response["artifact_compressed_bytes"] = json!(stats.compressed_bytes);
            response["artifact_original_bytes"] = json!(stats.original_bytes);
        }
        Err(e) => {
            eprintln!("[ap] artifact export failed (index succeeded): {e}");
            response["artifact_error"] = json!(e);
        }
    }
}

pub(crate) fn do_index_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let req = IndexRequest::prepare(args)?;

    // Issue #55/#62 bootstrap: when the caller opts in AND there is no local
    // graph yet, import the committed snapshot instead of cold-indexing. The
    // staleness contract lives in `attempt_bootstrap`; any path that does NOT
    // import falls through below carrying an assertable `bootstrap_skipped`.
    let mut bootstrap_skipped: Option<Value> = None;
    if req.want_bootstrap && !req.graph_dir.exists() {
        match attempt_bootstrap(
            &req.codebase,
            &req.output_dir,
            &req.graph_dir,
            &req.manifest_path,
            req.accept_stale,
            &req.options,
        ) {
            BootstrapOutcome::Imported(resp) => return Ok(resp),
            BootstrapOutcome::Reindex(note) => bootstrap_skipped = note,
        }
    }
    if let Some(resp) = try_incremental_index(&req) {
        return Ok(resp);
    }

    // lbug creates the database itself; a stale artifact from a prior run (a
    // directory OR a single-file db) must go first so it can initialise cleanly.
    if req.graph_dir.exists() {
        remove_stale_graph_artifact(&req.graph_dir)?;
    }
    let result =
        indexer::index_codebase_with_language(&req.codebase, &req.graph_dir, &req.options)?;
    Ok(full_index_response(
        &req,
        &result,
        bootstrap_skipped,
        persist_full_index_sidecars(&req, &result),
    ))
}

/// The `index_codebase` response for a completed full index, with the optional
/// sections a given call earns.
fn full_index_response(
    req: &IndexRequest,
    result: &indexer::IndexResult,
    bootstrap_skipped: Option<Value>,
    sidecars: (std::path::PathBuf, Value, Option<String>),
) -> Value {
    let (coverage_path, cochange_summary, meta_err) = sidecars;
    let mut response = json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_codebase",
        "graph_path": result.graph_path.to_string_lossy(),
        "node_count": result.node_count,
        "edge_count": result.edge_count,
        "files_indexed": result.files_indexed,
        "elapsed_ms": result.elapsed_ms,
    });
    if !cochange_summary.is_null() {
        response["cochange"] = cochange_summary;
    }
    response["coverage"] = coverage_summary(&result.coverage);
    if let Some(note) = bootstrap_skipped {
        response["bootstrap_skipped"] = note;
    }
    if req.want_export {
        attach_artifact_export(&mut response, req, result, &coverage_path);
    }
    report_meta_error(&mut response, meta_err);
    response
}

/// Max example files listed per coverage kind in a tool response. The counts are
/// always exact; the example lists are capped so a repo with thousands of
/// parse-incomplete files cannot blow the MCP tool-result budget. The full lists
/// live in the `index_coverage.json` sidecar and are queryable via index_status.
// source: DeusData/codebase-memory-mcp mcp.c — coverage responses carry "counts
// plus a FEW EXAMPLES only", the complete lists queryable separately.
pub(crate) const COVERAGE_LIST_CAP: usize = 50;

/// The coverage-honesty caveat repeated on every surface that reports coverage
/// (issue #57), mirroring the reference's wording so agents never overtrust it.
// source: DeusData/codebase-memory-mcp mcp.c — "absence of a flag is NOT a
// completeness guarantee; prefer grep inside flagged ranges".
pub(crate) const COVERAGE_CAVEAT: &str = "Best-effort signal, NOT a completeness guarantee: \
    absence of a flag only means the indexer detected no gap (a file keyed to a \
    subtly wrong grammar can still parse 'clean'). 'parse_incomplete' files WERE \
    indexed but constructs inside the flagged line ranges MAY be missing from the \
    graph — prefer grep there. 'skipped'/'quarantined' files are NOT in the graph \
    at all. source: DeusData/codebase-memory-mcp coverage wording.";

/// Renders a `CoverageReport` into an honest, budget-bounded JSON block: exact
/// counts per kind, capped example lists (parse_incomplete carries error ranges;
/// skipped/quarantined carry reasons), and the completeness caveat.
pub(crate) fn coverage_summary(report: &indexer::coverage::CoverageReport) -> Value {
    use indexer::coverage::CoverageKind;
    let (partial, skipped, quarantined) = report.counts();
    let mut partial_files = Vec::new();
    let mut skipped_files = Vec::new();
    let mut quarantined_files = Vec::new();
    // Issue #249: directories pruned by an explicit `exclude_dirs` match carry
    // a distinct count in the receipt, nested under `skipped` (they share its
    // "not indexed at all" kind) rather than a bespoke top-level bucket — the
    // `detail` field already carries the reason string per-file.
    let mut user_excluded_count: u64 = 0;
    for (rel, cov) in &report.files {
        match cov.kind {
            CoverageKind::ParsePartial => {
                if partial_files.len() < COVERAGE_LIST_CAP {
                    partial_files.push(json!({"path": rel, "error_ranges": cov.error_ranges}));
                }
            }
            CoverageKind::Skipped => {
                if cov.detail == "user_excluded" {
                    user_excluded_count += 1;
                }
                if skipped_files.len() < COVERAGE_LIST_CAP {
                    skipped_files.push(json!({"path": rel, "reason": cov.detail}));
                }
            }
            CoverageKind::Quarantined => {
                if quarantined_files.len() < COVERAGE_LIST_CAP {
                    quarantined_files.push(json!({"path": rel, "reason": cov.detail}));
                }
            }
        }
    }
    json!({
        "index_mode": report.index_mode,
        "files_indexed": report.files_indexed,
        "parse_incomplete": { "count": partial, "files": partial_files },
        "skipped": {
            "count": skipped,
            "user_excluded_count": user_excluded_count,
            "files": skipped_files
        },
        "quarantined": { "count": quarantined, "files": quarantined_files },
        "caveat": COVERAGE_CAVEAT,
    })
}

/// Loads the coverage sidecar for a graph at `graph_dir` (its `output_dir` is the
/// parent) and renders the summary, or `null` when no coverage is available.
pub(crate) fn coverage_summary_for_graph(graph_dir: &Path) -> Value {
    match graph_dir.parent() {
        Some(output_dir) => {
            let path = indexer::coverage::coverage_path(output_dir);
            match indexer::coverage::load(&path) {
                Some(report) => coverage_summary(&report),
                None => Value::Null,
            }
        }
        None => Value::Null,
    }
}

/// The `query_graph(graph="missed")` response (issue #57): a structural
/// enumeration of what the index does NOT fully cover, so an agent can pivot to
/// grep. `null` coverage means no coverage sidecar exists for this graph — which
/// is itself NOT a completeness claim (the index may predate coverage tracking).
pub(crate) fn query_missed_response(graph_path: &Path) -> Value {
    let coverage = coverage_summary_for_graph(graph_path);
    if coverage.is_null() {
        return json!({
            "stage": 3,
            "status": "ok",
            "tool": "query_graph",
            "graph": "missed",
            "coverage": Value::Null,
            "note": "No coverage sidecar for this graph (indexed before coverage \
                     tracking, or the sidecar was not carried alongside the graph). \
                     Absence of coverage data is NOT a completeness guarantee — \
                     re-index to generate it.",
        });
    }
    json!({
        "stage": 3,
        "status": "ok",
        "tool": "query_graph",
        "graph": "missed",
        "coverage": coverage,
        "note": "These files/ranges are where the index is known to be incomplete \
                 — prefer grep for them before trusting a negative graph result.",
    })
}

// ---------------------------------------------------------------------------
// Stage 3a — index_status (issue #57 coverage surface)
// ---------------------------------------------------------------------------

pub(crate) fn run_index_status(arguments: &Value) -> Value {
    match do_index_status(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "index_status_failed", "message": msg
        }),
    }
}

/// Reports a graph's indexing status: node/edge counts plus the coverage-honesty
/// report (issue #57) — files indexed, parse-incomplete (count + capped list with
/// ranges), and skipped/quarantined (count + list with reasons). This is the
/// authoritative coverage surface; the caveat states that absence of a flag is
/// not a completeness guarantee.
pub(crate) fn do_index_status(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let (node_count, edge_count) = graph_counts(graph_path);
    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_status",
        "graph_path": graph_str,
        "node_count": node_count,
        "edge_count": edge_count,
        "coverage": coverage_summary_for_graph(graph_path),
    }))
}
