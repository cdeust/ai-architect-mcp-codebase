//! Stage 3a — `index_codebase` / `index_status` / `ingest_traces` handler
//! logic. Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use crate::artifact;
use crate::cochange;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use crate::graph_cache;
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

pub(crate) fn do_index_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'path'")?;
    let output_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'output_dir'")?;
    let lang_filter = parse_language_filter(args)?;
    let dependency_scope = parse_dependency_scope(args)?;
    let exclude_dirs = parse_exclude_dirs(args)?;
    let options = indexer::IndexOptions {
        language_filter: lang_filter,
        dependency_scope,
        exclude_dirs,
    };
    let want_export = parse_bool_arg(args, "export_artifact", false)?;
    let want_bootstrap = parse_bool_arg(args, "bootstrap", false)?;
    let accept_stale = parse_bool_arg(args, "accept_stale", false)?;
    // Issue #62: force a from-scratch rebuild even when an incremental baseline
    // exists (e.g. after changing the language filter or dependency scope, which
    // the manifest does not capture). Default false → auto-incremental when a
    // prior index + manifest are present.
    let want_full = parse_bool_arg(args, "full", false)?;
    // Issue #58: mine git temporal coupling (FILE_CHANGES_WITH) after indexing.
    // Default true — it is cheap (one bounded `git log`) and the architect agent
    // reads churning pairs straight from the graph. Set false to skip (e.g. a
    // non-git tree, though mining self-skips there too).
    let want_cochange = parse_bool_arg(args, "cochange", true)?;

    let codebase = require_absolute(path_str, "path")?;
    if !codebase.exists() {
        return Err(format!("path does not exist: {}", codebase.display()));
    }
    // Issue #195: migrate the pre-rename artifact dir before anything below
    // walks the tree or reads it — this is the earliest touchpoint for a
    // full-index call, and `artifact::migrate_legacy_dir` is a cheap no-op
    // once migrated.
    artifact::migrate_legacy_dir(&codebase);
    let output_dir = require_absolute(output_str, "output_dir")?;
    fs::create_dir_all(&output_dir).map_err(|e| format!("create output dir: {e}"))?;
    let graph_dir = output_dir.join("graph");
    let manifest_path = indexer::manifest::manifest_path(&output_dir);
    // source: H4 fix — validate the derived path ends in `/graph` and is not
    // a forbidden system root before any destructive op.
    validate_graph_path_safe(&graph_dir)?;

    // Issue #55/#62 bootstrap: when the caller opts in AND there is no local
    // graph yet, import the committed snapshot instead of cold-indexing. The
    // evolved staleness contract (fresh → import; stale → import THEN incremental
    // fill by DEFAULT; stale + accept_stale → import as-is, skip the fill;
    // import/fill failure → reindex) lives in `attempt_bootstrap`. Any path that
    // does NOT import falls through to the full index below, carrying an
    // assertable `bootstrap_skipped` note.
    let mut bootstrap_skipped: Option<Value> = None;
    if want_bootstrap && !graph_dir.exists() {
        match attempt_bootstrap(
            &codebase,
            &output_dir,
            &graph_dir,
            &manifest_path,
            accept_stale,
            &options,
        ) {
            BootstrapOutcome::Imported(resp) => return Ok(resp),
            BootstrapOutcome::Reindex(note) => bootstrap_skipped = note,
        }
    }

    // Issue #62 incremental mode: when a prior local graph AND a readable file
    // manifest exist and the caller did not force `full`, re-index only the
    // changed files. Any failure logs loudly and falls through to a full
    // rebuild (§13 — no silent path). Auto-selected so the common "re-index
    // after editing a few files" call pays work proportional to the diff.
    if !want_full && graph_dir.exists() {
        if let Some(prior) = indexer::manifest::load(&manifest_path) {
            match indexer::index_incremental(
                &codebase,
                &graph_dir,
                &manifest_path,
                &options,
                &prior,
            ) {
                Ok(inc) => {
                    write_graph_meta(&output_dir, &codebase);
                    return Ok(finish_incremental_response(
                        inc,
                        &graph_dir,
                        &codebase,
                        &manifest_path,
                        want_export,
                        want_cochange,
                    ));
                }
                Err(e) => {
                    eprintln!(
                        "[ap] incremental index failed ({e}); falling back to a full re-index"
                    );
                }
            }
        }
    }

    // lbug creates the database itself; if a stale graph artifact exists from a
    // prior run, remove it so lbug can initialise cleanly.
    if graph_dir.exists() {
        // Prior run may have left a dir OR a single-file Kuzu db; remove either.
        remove_stale_graph_artifact(&graph_dir)?;
    }

    let result = indexer::index_codebase_with_language(&codebase, &graph_dir, &options)?;
    // Record the absolute source root beside the graph (relative file paths
    // stay in the graph; the root lets consumers reconstruct absolute paths).
    write_graph_meta(&output_dir, &codebase);
    // Issue #62: persist the per-file manifest so the NEXT index_codebase call
    // can run incrementally. Best-effort — a failed manifest write only forces
    // the next run to full-index, never fails this one.
    if let Err(e) = indexer::write_full_manifest(&codebase, &manifest_path, &options) {
        eprintln!("[ap] file manifest write failed (index succeeded): {e}");
    }
    // Issue #57: persist the coverage-honesty sidecar (parse-incomplete, skipped,
    // and quarantined files). Best-effort — a failed write only drops the honesty
    // signal, never the index.
    let coverage_path = indexer::coverage::coverage_path(&output_dir);
    if let Err(e) = indexer::coverage::save(&coverage_path, &result.coverage) {
        eprintln!("[ap] coverage sidecar write failed (index succeeded): {e}");
    }
    // Issue #58: mine git temporal coupling into FILE_CHANGES_WITH edges. Full
    // index → full re-mine of the window.
    let cochange_summary = if want_cochange {
        run_cochange(&graph_dir, &codebase, &output_dir, cochange::Mode::Full)
    } else {
        Value::Null
    };

    // Issue #55: an explicit index is the single (best-ratio, zstd-9) export
    // tier. Failure to export is LOUD but non-fatal — the index itself succeeded.
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
    if want_export {
        match artifact::export_artifact(
            &graph_dir,
            &codebase,
            result.node_count,
            result.edge_count,
            Some(&manifest_path),
            Some(&coverage_path),
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
    Ok(response)
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

// ---------------------------------------------------------------------------
// Stage 3 — ingest_traces (issue #58 runtime enrichment)
// ---------------------------------------------------------------------------

pub(crate) fn run_ingest_traces(arguments: &Value) -> Value {
    match do_ingest_traces(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "ingest_traces_failed", "message": msg
        }),
    }
}

/// Node labels a runtime trace endpoint may resolve to (callables).
pub(crate) const CALLABLE_LABELS: &[&str] = &["Function", "Method"];

/// Folds runtime caller→callee observations into the graph (issue #58): annotate
/// a matching static Calls edge with `observed_count`, or create an
/// `OBSERVED_CALLS` edge where static resolution found none (the divergence
/// signal — runtime truth the static analysis missed).
///
/// Input: `{graph_path, traces: [{caller, callee, count}]}`. `caller`/`callee`
/// are qualified names (`file::symbol`); `count` defaults to 1. Postconditions:
/// each trace either annotated a static Calls edge (matched), created/updated an
/// OBSERVED_CALLS edge (unmatched_created), or was recorded as unresolved
/// (endpoint not a Function/Method node). The response reports the three counts
/// plus a capped list of the created divergences and the unresolved names.
pub(crate) fn do_ingest_traces(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let traces = args
        .get("traces")
        .and_then(|v| v.as_array())
        .ok_or("missing required field 'traces' (array of {caller, callee, count})")?;

    // Aggregate observations per (caller, callee) so repeated pairs sum instead
    // of creating duplicate edges.
    let mut agg: std::collections::BTreeMap<(String, String), i64> =
        std::collections::BTreeMap::new();
    for t in traces {
        let caller = t.get("caller").and_then(|v| v.as_str());
        let callee = t.get("callee").and_then(|v| v.as_str());
        let (caller, callee) = match (caller, callee) {
            (Some(a), Some(b)) => (a.to_string(), b.to_string()),
            _ => return Err("each trace needs 'caller' and 'callee' strings".to_string()),
        };
        let count = t.get("count").and_then(|v| v.as_i64()).unwrap_or(1).max(1);
        *agg.entry((caller, callee)).or_insert(0) += count;
    }

    let store = graph_cache::open_cached(graph_path)?;
    let mut matched = 0u64;
    let mut unmatched_created = 0u64;
    let mut unresolved: Vec<String> = Vec::new();
    let mut divergences: Vec<Value> = Vec::new();

    for ((caller, callee), count) in agg {
        let from_label = callable_label(&store, &caller);
        let to_label = callable_label(&store, &callee);
        let (from_label, to_label) = match (from_label, to_label) {
            (Some(f), Some(t)) => (f, t),
            _ => {
                if from_label.is_none() {
                    unresolved.push(caller.clone());
                }
                if to_label.is_none() {
                    unresolved.push(callee.clone());
                }
                continue;
            }
        };
        let static_table = format!("Calls_{from_label}_{to_label}");
        if annotate_static_call(&store, &static_table, &caller, &callee, count)? {
            matched += 1;
        } else {
            let observed_table = format!("OBSERVED_CALLS_{from_label}_{to_label}");
            upsert_observed_call(&store, &observed_table, &caller, &callee, count)?;
            unmatched_created += 1;
            if divergences.len() < COVERAGE_LIST_CAP {
                divergences.push(json!({
                    "caller": caller, "callee": callee, "observed_count": count
                }));
            }
        }
    }

    unresolved.sort();
    unresolved.dedup();
    let unresolved_total = unresolved.len();
    unresolved.truncate(COVERAGE_LIST_CAP);

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "ingest_traces",
        "matched": matched,
        "unmatched_created": unmatched_created,
        "unresolved_names": unresolved_total,
        "unresolved_names_sample": unresolved,
        "observed_divergences": divergences,
        "note": "matched = a static Calls edge annotated with observed_count. \
                 unmatched_created = OBSERVED_CALLS edges where runtime saw a call \
                 static analysis did not — the valuable divergence signal. \
                 unresolved_names = trace endpoints that are not Function/Method \
                 nodes (wrong qualified name, or an uncovered/unindexed symbol).",
    }))
}
