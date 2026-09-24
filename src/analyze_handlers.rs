//! Stage 3 — `analyze_codebase` (index+resolve+cluster in one call),
//! Stage 3b-v2 `lsp_resolve`, and Stage 3e `detect_changes` handler logic.
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use crate::epistemic;
use crate::search;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use crate::clustering;
use crate::git_diff;
use crate::graph_cache;
use crate::graph_store;
use crate::handler_util::*;
use crate::indexer;
use crate::lsp_resolver;
use crate::query_handlers::*;
use crate::resolver;

mod lsp_coverage;
mod lsp_durability;
mod lsp_outcome;
use lsp_outcome::LspOutcome;

mod lsp_tool;
// `do_lsp_resolve` is not re-exported: it has no caller outside `lsp_tool`
// (only `run_lsp_resolve` invokes it, from within the same module), so
// re-exporting it here would be an unused import. It stays reachable
// crate-wide at its own path (`analyze_handlers::lsp_tool::do_lsp_resolve`)
// via its own `pub(crate)`, unchanged from before the split.
pub(crate) use lsp_tool::{detect_dominant_language, run_lsp_resolve};

// ---------------------------------------------------------------------------
// Stage 3 — analyze_codebase (all-in-one: index + resolve + cluster)
// ---------------------------------------------------------------------------

pub(crate) fn run_analyze_codebase(arguments: &Value) -> Value {
    match do_analyze_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "analyze_failed", "message": msg
        }),
    }
}

/// The caller-supplied half of an `analyze_codebase` call, validated once and
/// its output directory prepared.
///
/// A parameter object (§4.4): the four settings and the three derived paths are
/// resolved together and used together by all four phases.
struct AnalyzeRequest {
    codebase: std::path::PathBuf,
    output_dir: std::path::PathBuf,
    graph_dir: std::path::PathBuf,
    options: indexer::IndexOptions,
    lang_filter: Option<crate::parser::Language>,
    gamma: f64,
    enable_lsp: bool,
}

impl AnalyzeRequest {
    /// Touches the filesystem deliberately — hence `prepare`, not `parse`:
    /// `validate_graph_path_safe` must run before the stale-artifact removal
    /// below it (source: H4 fix — see do_index_codebase).
    fn prepare(args: &serde_json::Map<String, Value>) -> Result<Self, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'path'")?;
        let output_str = args
            .get("output_dir")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'output_dir'")?;
        let lang_filter = parse_language_filter(args)?;
        let codebase = require_absolute(path_str, "path")?;
        if !codebase.exists() {
            return Err(format!("path does not exist: {}", codebase.display()));
        }
        let output_dir = require_absolute(output_str, "output_dir")?;
        fs::create_dir_all(&output_dir).map_err(|e| format!("create output dir: {e}"))?;
        let graph_dir = output_dir.join("graph");
        validate_graph_path_safe(&graph_dir)?;
        Ok(AnalyzeRequest {
            codebase,
            output_dir,
            graph_dir,
            options: indexer::IndexOptions {
                language_filter: lang_filter,
                dependency_scope: parse_dependency_scope(args)?,
                exclude_dirs: parse_exclude_dirs(args)?,
            },
            lang_filter,
            gamma: args
                .get("resolution_param")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0),
            enable_lsp: args.get("lsp").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }
}

/// Phase 2b, optional. Preserve the failure reason while analysis continues.
/// The resolver can fail after edge writes, so fallback may include partial LSP.
/// Source: analyze_lsp_status stdio regression — `.ok()` made a missing
/// requested language server indistinguishable from a disabled LSP phase.
fn lsp_phase(req: &AnalyzeRequest, store: &graph_store::GraphStore) -> LspOutcome {
    if !req.enable_lsp {
        return LspOutcome::Disabled;
    }
    let effective_lang = match req.lang_filter {
        Some(lang) => lang.as_str().to_string(),
        None => detect_dominant_language(&req.codebase),
    };
    match lsp_resolver::resolve_with_lsp(
        store,
        &req.codebase,
        &effective_lang,
        None,
        std::time::Duration::from_secs(30),
    ) {
        Ok(result) => LspOutcome::Completed(result),
        Err(error) => LspOutcome::Failed(error),
    }
}

/// Closes the analysis handle. When the LSP phase completed, reopens the graph
/// and compares its `lsp-definition` rows with what the phase wrote; a loss
/// turns the phase into `Failed("lsp_rows_not_durable: ...")` so the response
/// says so instead of reporting `completed`.
fn confirm_lsp_rows(
    outcome: LspOutcome,
    store: graph_store::GraphStore,
    graph_path: &Path,
) -> LspOutcome {
    match outcome {
        LspOutcome::Completed(result) => {
            match lsp_durability::verify_after_reopen(store, graph_path) {
                Ok(_) => LspOutcome::Completed(result),
                Err(error) => LspOutcome::Failed(error),
            }
        }
        other => other,
    }
}

/// Node and relationship totals of the finished graph, read after the last
/// phase. `index.*` above is the snapshot the index phase took before resolve
/// wrote its edges, so it is not the size of the graph `index_status` reports;
/// this block is, field for field (issue #338). When the graph cannot be read
/// back the block carries the reason under `error` instead of counts.
fn graph_totals(graph_dir: &std::path::Path) -> Value {
    match crate::history_handlers::try_graph_counts(graph_dir) {
        Ok(c) => json!({
            "node_count": c.nodes,
            "edge_count": c.edges,
            "call_site_target_count": c.call_site_targets,
        }),
        Err(e) => json!({ "error": e }),
    }
}

/// The four phases' counts, as one response.
fn analyze_envelope(
    index_result: &indexer::IndexResult,
    resolve_result: &resolver::ResolutionResult,
    cluster_result: &clustering::ClusteringResult,
    search_index_result: &search::SearchIndexResult,
    lsp_result: &LspOutcome,
    total_ms: u64,
) -> Value {
    json!({
        "stage": 3,
        "status": "ok",
        "tool": "analyze_codebase",
        "graph_path": index_result.graph_path.to_string_lossy(),
        "index": {
            "node_count": index_result.node_count,
            "edge_count": index_result.edge_count,
            "files_indexed": index_result.files_indexed,
        },
        "graph": graph_totals(&index_result.graph_path),
        "resolve": {
            "phase": "static",
            "total_edges": resolve_result.total_edges,
            "no_call_macro_sites": resolve_result.no_call_macro_sites,
            "resolution_rate": format!("{:.2}",
                if resolve_result.total_refs > 0 {
                    resolve_result.total_edges as f64 / resolve_result.total_refs as f64
                } else { 0.0 }),
        },
        "cluster": {
            "community_count": cluster_result.communities,
            "modularity": format!("{:.6}", cluster_result.modularity),
            "process_count": cluster_result.processes,
        },
        "search_index": {
            "bm25_doc_count": search_index_result.bm25_doc_count,
            "vector_doc_count": search_index_result.vector_doc_count,
            "elapsed_ms": search_index_result.elapsed_ms,
        },
        "lsp_resolve": lsp_result.counts(),
        "lsp_status": lsp_result.status(),
        "total_elapsed_ms": total_ms,
    })
}

pub(crate) fn do_analyze_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let req = AnalyzeRequest::prepare(args)?;
    if req.graph_dir.exists() {
        // Prior run may have left a dir OR a single-file Kuzu db; remove either.
        remove_stale_graph_artifact(&req.graph_dir)?;
    }
    let total_start = std::time::Instant::now();

    // Phase 1: index, then coverage + manifest before the metadata commit point.
    let mut index_result =
        indexer::index_codebase_with_language(&req.codebase, &req.graph_dir, &req.options)?;
    let sidecar_err = persist_analyze_sidecars(&req, &index_result.coverage)?;
    // Phase 2: resolve, then optionally refine with an LSP.
    let store = graph_store::GraphStore::open_or_create(&index_result.graph_path)?;
    let resolve_result = resolver::resolve_graph(&store)?;
    let lsp_result = lsp_phase(&req, &store);
    fold_lsp_coverage(&req, &mut index_result.coverage, &lsp_result)?;
    // Phase 3: cluster. Phase 4: build the BM25 + TF-IDF search index.
    let cluster_result = clustering::cluster_graph(&store, req.gamma)?;
    let search_index_result = search::build_search_index(&store, &req.output_dir, &req.codebase)?;
    // Issue #352: the same durability check `lsp_resolve` makes, at the end of
    // the analysis, when nothing else will write through this handle.
    let lsp_result = confirm_lsp_rows(lsp_result, store, &index_result.graph_path);

    let mut response = analyze_envelope(
        &index_result,
        &resolve_result,
        &cluster_result,
        &search_index_result,
        &lsp_result,
        total_start.elapsed().as_millis() as u64,
    );
    response["coverage"] = crate::indexing_handlers::coverage_summary(&index_result.coverage);
    report_sidecar_error(&mut response, sidecar_err);
    Ok(response)
}

/// Writes coverage and the manifest before `meta.json`, the commit point.
/// Coverage is replaced on every full analysis, including an empty gap map.
///
/// `analyze_codebase` used to write `meta.json` and NO manifest at all
/// (fleet-watch#112 review round 6). It and `index_codebase` are documented as
/// interchangeable entry points over the same `output_dir`, so running analyze
/// where a previous index had left a manifest froze that manifest in place:
/// every file added afterwards stayed permanently invisible to `count_dirty`,
/// and the graph reported fresh while missing them. Writing no manifest at all
/// is equally wrong in the other direction — the freshness check then has
/// nothing to compare and can never answer.
///
/// Manifest/meta writes remain best-effort and surface their error. A coverage
/// write failure propagates so a stale or missing receipt cannot be reported
/// as a successful analysis.
fn persist_analyze_sidecars(
    req: &AnalyzeRequest,
    coverage: &indexer::coverage::CoverageReport,
) -> Result<Option<String>, String> {
    // Source: dy-wcet stdio reproduction (2026-09-06): analyze discarded
    // IndexResult.coverage, leaving query_graph(graph="missed") unavailable
    // or stale. A failed write must not return a successful coverage receipt.
    indexer::coverage::save(&indexer::coverage::coverage_path(&req.output_dir), coverage)?;
    let manifest_path = indexer::manifest::manifest_path(&req.output_dir);
    if let Err(e) = indexer::write_full_manifest(&req.codebase, &manifest_path, &req.options) {
        eprintln!("[ap] file manifest write failed (analyze succeeded): {e}");
        return Ok(Some(e));
    }
    Ok(write_graph_meta(&req.output_dir, &req.codebase)
        .err()
        .inspect(|e| {
            eprintln!("[ap] graph meta sidecar write failed (analyze succeeded): {e}");
        }))
}

/// Issue #292: the sidecar was written before the LSP phase, so fold its
/// `unlinked-file` verdicts in and write it again — with the same "a failed
/// coverage write fails the analysis" rule as `persist_analyze_sidecars`.
fn fold_lsp_coverage(
    req: &AnalyzeRequest,
    coverage: &mut indexer::coverage::CoverageReport,
    lsp_result: &LspOutcome,
) -> Result<(), String> {
    let LspOutcome::Completed(result) = lsp_result else {
        return Ok(());
    };
    if lsp_coverage::merge_unlinked(coverage, &result.unlinked_check) {
        indexer::coverage::save(&indexer::coverage::coverage_path(&req.output_dir), coverage)?;
    }
    Ok(())
}

/// Surfaces a failed sidecar write on the response rather than leaving the
/// caller believing a complete, queryable graph landed (review round 6 finding
/// 4: three of five `write_graph_meta` call sites swallowed this).
fn report_sidecar_error(response: &mut Value, err: Option<String>) {
    if let Some(err) = err {
        response["meta_write_error"] = json!(err);
    }
}

// ---------------------------------------------------------------------------
// Stage 3e — detect_changes (git diff impact)
// ---------------------------------------------------------------------------

pub(crate) fn run_detect_changes(arguments: &Value) -> Value {
    match do_detect_changes(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "detect_changes_failed", "message": msg
        }),
    }
}

pub(crate) fn do_detect_changes(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let diff_text = args.get("diff_text").and_then(|v| v.as_str());
    let codebase_path = args.get("codebase_path").and_then(|v| v.as_str());
    let base_ref = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD~1");
    let head_ref = args
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;

    let analysis = if let Some(text) = diff_text {
        git_diff::analyze_diff(&store, text)?
    } else if let Some(repo) = codebase_path {
        let repo_path = Path::new(repo);
        if !repo_path.exists() {
            return Err(format!("codebase_path does not exist: {repo}"));
        }
        git_diff::analyze_git_diff(&store, repo_path, base_ref, head_ref)?
    } else {
        return Err("either 'diff_text' or 'codebase_path' must be provided".to_string());
    };

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "detect_changes",
        "files_changed": analysis.files_changed,
        "symbols_affected": analysis.symbols_affected,
        "symbols_affected_count": analysis.symbols_affected.len(),
        "communities_affected": analysis.communities_affected,
        "communities_affected_count": analysis.communities_affected.len(),
        "processes_affected": analysis.processes_affected,
        "processes_affected_count": analysis.processes_affected.len(),
        "risk_score": format!("{:.4}", analysis.risk_score),
        // Epistemic qualification of risk_score: the mean confidence of the
        // reverse-dependency edges the risk rests on, and whether any changed
        // symbol's blast radius is a lower bound (true risk may exceed score).
        // source: git_diff::assess_dependency_confidence.
        "mean_dependency_confidence": format!("{:.2}", analysis.mean_dependency_confidence),
        "epistemic": analysis.epistemic,
        "epistemic_reasons": analysis.epistemic_reasons,
        "next_steps": detect_changes_next_steps(&analysis),
    }))
}

/// Suggests follow-up tools after a `detect_changes` result. Graph-grounded:
/// each hint is gated on a present dimension of the analysis.
pub(crate) fn detect_changes_next_steps(analysis: &git_diff::DiffAnalysis) -> Value {
    let mut steps = Vec::new();
    if !analysis.symbols_affected.is_empty() {
        steps.push(
            "drill into a changed symbol's blast radius: get_impact on a \
             `symbols_affected[].qualified_name`"
                .to_string(),
        );
    }
    if analysis.epistemic == epistemic::Boundary::LowerBound.as_str() {
        steps.push(
            "risk is a lower bound (see `epistemic_reasons`) — run lsp_resolve to \
             tighten dynamic-dispatch edges before trusting the score"
                .to_string(),
        );
    }
    json!(steps)
}

// ---------------------------------------------------------------------------
// Stage 4 — prepare_prd_input (bundle verified finding + graph intel)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod confirm_lsp_rows_tests {
    use super::*;
    use crate::lsp_client::{LspResolutionResult, ServerHealth, UnlinkedFileCheck};

    // The LSP phase of analyze_codebase does not go through `do_lsp_resolve`,
    // so it has its own call of the durability check (issue #352).

    fn completed() -> LspOutcome {
        LspOutcome::Completed(LspResolutionResult {
            resolved_count: 0,
            failed_count: 0,
            skipped_count: 0,
            outside_targets_count: 0,
            macro_sites_count: 0,
            elapsed_ms: 0,
            server_health: ServerHealth::not_probed(),
            unlinked_check: UnlinkedFileCheck::default(),
        })
    }

    fn graph() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        graph_store::GraphStore,
    ) {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("g");
        let store = graph_store::GraphStore::open_or_create(&path).expect("open");
        store.create_schema().expect("schema");
        (tmp, path, store)
    }

    #[test]
    fn a_completed_phase_whose_rows_are_durable_stays_completed() {
        let (_tmp, path, store) = graph();
        for id in ["a", "b"] {
            store
                .execute_query(&format!("CREATE (:Function {{id: '{id}', name: '{id}'}})"))
                .expect("node");
        }
        store
            .insert_edge_if_absent(
                "Calls_Function_Function",
                "a",
                "b",
                &[
                    ("confidence", "0.9"),
                    ("resolution_method", "'lsp-definition'"),
                ],
            )
            .expect("edge");
        assert!(matches!(
            confirm_lsp_rows(completed(), store, &path),
            LspOutcome::Completed(_)
        ));
    }

    #[test]
    fn a_phase_that_did_not_complete_passes_through_and_releases_the_handle() {
        let (_tmp, path, store) = graph();
        assert!(matches!(
            confirm_lsp_rows(LspOutcome::Disabled, store, &path),
            LspOutcome::Disabled
        ));
        let (_tmp2, path2, store2) = graph();
        match confirm_lsp_rows(LspOutcome::Failed("boom".into()), store2, &path2) {
            LspOutcome::Failed(e) => assert_eq!(e, "boom"),
            _ => panic!("a failure must pass through unchanged"),
        }
    }

    #[test]
    fn a_completed_phase_on_a_graph_that_cannot_be_read_back_fails_loudly() {
        let (tmp, path, store) = graph();
        drop(tmp); // the graph directory is gone before the check reopens it
        match confirm_lsp_rows(completed(), store, &path) {
            LspOutcome::Failed(e) => assert!(!e.is_empty()),
            _ => panic!("an unreadable graph must not read as completed"),
        }
    }
}
