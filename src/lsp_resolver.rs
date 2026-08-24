// lsp_resolver — LSP-enhanced resolution pass for unresolved call sites.
//
// Queries a Language Server Protocol server for textDocument/definition
// to resolve method calls on inferred types that the static 3b resolver
// cannot handle. Runs AFTER resolve_graph as an optional enhancement.
//
// source: stages/stage-3b.md §7 — "method calls on inferred types" deferred to LSP

use crate::graph_store::GraphStore;
use crate::lsp_client::{self, LspClient, LspResolutionResult};
use std::path::Path;
use std::time::{Duration, Instant};

mod edges;
mod pass;
mod sites;

use edges::SiteContext;
use pass::LspPass;
use sites::{
    build_node_position_index, collect_unresolved_callsites, group_by_file, language_id_for,
    UnresolvedCallSite,
};

/// Budget reserved for the in-flight request when deciding whether another one
/// still fits inside `timeout`.
/// source: `LspClient`'s own per-request read deadline (lsp_client.rs).
const PER_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run LSP-enhanced resolution on unresolved call sites in the graph.
///
/// 1. Collects CallSite nodes that lack a Calls edge (unresolved).
/// 2. Starts the appropriate LSP server.
/// 3. For each unresolved site, queries textDocument/definition.
/// 4. Maps definition locations back to graph nodes, adds Calls edges.
pub fn resolve_with_lsp(
    store: &GraphStore,
    codebase_path: &Path,
    language: &str,
    lsp_command_override: Option<&str>,
    timeout: Duration,
) -> Result<LspResolutionResult, String> {
    let start = Instant::now();
    let (cmd, default_args) = lsp_command(language, lsp_command_override)?;
    if !lsp_client::is_command_available(cmd) {
        return Err(format!("lsp_not_found: {cmd} not found in PATH"));
    }

    let unresolved = collect_unresolved_callsites(store)?;
    if unresolved.is_empty() {
        return Ok(LspResolutionResult {
            resolved_count: 0,
            failed_count: 0,
            skipped_count: 0,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
    }

    let mut client = LspClient::start(cmd, default_args, codebase_path, timeout)?;
    client.initialize(codebase_path)?;

    // fleet-watch#18: definition URIs come back absolute (and on macOS the
    // server may answer under /private/var while the caller passed /var, or
    // vice versa). Canonicalize the root ONCE so uri_to_relative_path can
    // strip it from the canonicalized definition path and recover the same
    // root-relative key the indexer used.
    let canonical_root =
        std::fs::canonicalize(codebase_path).unwrap_or_else(|_| codebase_path.to_path_buf());
    let node_index = build_node_position_index(store)?;
    let plan = PassPlan {
        codebase_path,
        language,
        deadline: timeout.saturating_sub(PER_REQUEST_TIMEOUT),
        ctx: SiteContext {
            node_index: &node_index,
            canonical_root: &canonical_root,
        },
    };

    let pass = drive_pass(store, &mut client, &plan, &unresolved, start);
    let _ = client.shutdown();
    pass.mark_resolved(store)?;
    Ok(pass.into_result(start.elapsed().as_millis() as u64))
}

/// Everything the per-file loop needs that is fixed for the whole pass,
/// grouped so `drive_pass` stays within the §4.4 parameter cap.
struct PassPlan<'a> {
    codebase_path: &'a Path,
    language: &'a str,
    /// Elapsed time after which no further request is issued.
    deadline: Duration,
    ctx: SiteContext<'a>,
}

/// Asks the server about every unresolved site, file by file, until the sites
/// run out or the budget does.
fn drive_pass(
    store: &GraphStore,
    client: &mut LspClient,
    plan: &PassPlan<'_>,
    unresolved: &[UnresolvedCallSite],
    start: Instant,
) -> LspPass {
    let mut pass = LspPass::new(unresolved.len());
    // Grouped by file so one `didOpen` serves every site in it.
    'files: for (file_path, sites) in &group_by_file(unresolved) {
        let abs_path = plan.codebase_path.join(file_path);
        let file_uri = lsp_client::path_to_file_uri(&abs_path);
        let Ok(content) = std::fs::read_to_string(&abs_path) else {
            // Missing or unreadable: its sites go unanswered, which the
            // skipped identity accounts for without a per-branch counter.
            continue;
        };
        if client
            .did_open(&file_uri, language_id_for(plan.language), &content)
            .is_err()
        {
            continue;
        }

        for site in sites {
            pass.record(
                store,
                site,
                client.get_definition(&file_uri, site.line, site.col),
                &plan.ctx,
            );
            // Respect the overall budget. Breaking out of BOTH loops matters:
            // continuing to the next file spent one more `didOpen` plus one
            // more definition request per remaining file, for a budget that
            // was already exhausted.
            if start.elapsed() > plan.deadline {
                break 'files;
            }
        }
    }
    pass
}

/// Resolves which language server to run.
///
/// source: C3 fix — a caller-provided `lsp_command_override` must be validated
/// against the allowlist BEFORE `Command::new`, to prevent arbitrary binary
/// execution. `LspClient::start` also validates, as defense in depth.
fn lsp_command<'a>(
    language: &str,
    override_cmd: Option<&'a str>,
) -> Result<(&'a str, &'static [&'static str]), String> {
    match override_cmd {
        Some(c) => {
            lsp_client::validate_lsp_command(c)?;
            Ok((c, &[]))
        }
        None => lsp_client::detect_lsp_command(language)
            .ok_or(format!("no LSP server known for language '{language}'")),
    }
}
