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
use crate::lsp_client;
use crate::lsp_resolver;
use crate::query_handlers::*;
use crate::resolver;

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

pub(crate) fn do_analyze_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'path'")?;
    let output_str = args
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'output_dir'")?;
    let gamma = args
        .get("resolution_param")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let enable_lsp = args.get("lsp").and_then(|v| v.as_bool()).unwrap_or(false);
    let lang_filter = parse_language_filter(args)?;
    let dependency_scope = parse_dependency_scope(args)?;
    let exclude_dirs = parse_exclude_dirs(args)?;
    let options = indexer::IndexOptions {
        language_filter: lang_filter,
        dependency_scope,
        exclude_dirs,
    };

    let codebase = require_absolute(path_str, "path")?;
    if !codebase.exists() {
        return Err(format!("path does not exist: {}", codebase.display()));
    }
    let output_dir = require_absolute(output_str, "output_dir")?;
    fs::create_dir_all(&output_dir).map_err(|e| format!("create output dir: {e}"))?;
    let graph_dir = output_dir.join("graph");
    // source: H4 fix — see do_index_codebase.
    validate_graph_path_safe(&graph_dir)?;
    if graph_dir.exists() {
        // Prior run may have left a dir OR a single-file Kuzu db; remove either.
        remove_stale_graph_artifact(&graph_dir)?;
    }

    let total_start = std::time::Instant::now();

    // Phase 1: index
    let index_result = indexer::index_codebase_with_language(&codebase, &graph_dir, &options)?;
    // Record the absolute source root beside the graph (see write_graph_meta).
    write_graph_meta(&output_dir, &codebase);

    // Phase 2: resolve
    let store = graph_store::GraphStore::open_or_create(&index_result.graph_path)?;
    let resolve_result = resolver::resolve_graph(&store)?;

    // Phase 2b: LSP-enhanced resolution (optional)
    let lsp_result = if enable_lsp {
        let effective_lang = match lang_filter {
            Some(lang) => lang.as_str().to_string(),
            None => detect_dominant_language(&codebase),
        };
        // graceful fallback: any LSP error is silently ignored below
        lsp_resolver::resolve_with_lsp(
            &store,
            &codebase,
            &effective_lang,
            None,
            std::time::Duration::from_secs(30),
        )
        .ok()
    } else {
        None
    };

    // Phase 3: cluster
    let cluster_result = clustering::cluster_graph(&store, gamma)?;

    // Phase 4: build search index (BM25 + TF-IDF vectors)
    let search_index_result = search::build_search_index(&store, &output_dir, &codebase)?;

    let total_ms = total_start.elapsed().as_millis() as u64;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "analyze_codebase",
        "graph_path": index_result.graph_path.to_string_lossy(),
        "index": {
            "node_count": index_result.node_count,
            "edge_count": index_result.edge_count,
            "files_indexed": index_result.files_indexed,
        },
        "resolve": {
            "total_edges": resolve_result.total_edges,
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
        "lsp_resolve": match &lsp_result {
            Some(r) => json!({
                "resolved_count": r.resolved_count,
                "failed_count": r.failed_count,
                "skipped_count": r.skipped_count,
                "elapsed_ms": r.elapsed_ms,
            }),
            None => json!(null),
        },
        "total_elapsed_ms": total_ms,
    }))
}

// ---------------------------------------------------------------------------
// Stage 3e — detect_changes (git diff impact)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Stage 3b-v2 — lsp_resolve (LSP-enhanced resolution)
// ---------------------------------------------------------------------------

pub(crate) fn run_lsp_resolve(arguments: &Value) -> Value {
    match do_lsp_resolve(arguments) {
        Ok(v) => v,
        Err(msg) => {
            // Distinguish specific failure reasons so callers can act on them.
            if msg.contains("lsp_command_not_allowed") {
                // source: C3 fix — surface the reason code plus the allowlist
                // so the caller knows which commands are accepted.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_command_not_allowed",
                    "message": msg,
                    "allowed": lsp_client::LSP_COMMAND_ALLOWLIST,
                })
            } else if msg.contains("lsp_not_found") {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_not_found",
                    "message": msg
                })
            } else if msg.contains("lsp_probe_failed") {
                // source: C-correctness bug 1 — binary on PATH but doesn't
                // speak LSP (rustup proxy, stub script, /bin/true, ...).
                // Distinct from lsp_not_found so callers can act on it.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_probe_failed",
                    "message": msg
                })
            } else {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_resolve_failed",
                    "message": msg
                })
            }
        }
    }
}

pub(crate) fn do_lsp_resolve(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let codebase_str = args
        .get("codebase_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'codebase_path'")?;
    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    let lsp_command = args.get("lsp_command").and_then(|v| v.as_str());
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30000);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let codebase_path = Path::new(codebase_str);
    if !codebase_path.exists() {
        return Err(format!("codebase_path does not exist: {codebase_str}"));
    }

    // Auto-detect language from codebase if needed
    let effective_lang = if language == "auto" {
        detect_dominant_language(codebase_path)
    } else {
        language.to_string()
    };

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let timeout = std::time::Duration::from_millis(timeout_ms);

    let result = lsp_resolver::resolve_with_lsp(
        &store,
        codebase_path,
        &effective_lang,
        lsp_command,
        timeout,
    )?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "lsp_resolve",
        "resolved_count": result.resolved_count,
        "failed_count": result.failed_count,
        "skipped_count": result.skipped_count,
        "elapsed_ms": result.elapsed_ms,
    }))
}

/// Detect the dominant language from file extensions in a codebase.
pub(crate) fn detect_dominant_language(path: &Path) -> String {
    let mut rs_count = 0u32;
    let mut py_count = 0u32;
    let mut ts_count = 0u32;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            match p.extension().and_then(|e| e.to_str()) {
                Some("rs") => rs_count += 1,
                Some("py") => py_count += 1,
                Some("ts") | Some("tsx") => ts_count += 1,
                _ => {}
            }
        }
    }

    if rs_count >= py_count && rs_count >= ts_count {
        "rust".to_string()
    } else if py_count >= ts_count {
        "python".to_string()
    } else {
        "typescript".to_string()
    }
}

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
