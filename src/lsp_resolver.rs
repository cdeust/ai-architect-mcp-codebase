// lsp_resolver — LSP-enhanced resolution pass for unresolved call sites.
//
// Queries a Language Server Protocol server for textDocument/definition
// to resolve method calls on inferred types that the static 3b resolver
// cannot handle. Runs AFTER resolve_graph as an optional enhancement.
//
// source: stages/stage-3b.md §7 — "method calls on inferred types" deferred to LSP

use crate::graph_store::GraphStore;
use crate::indexer::cargo_targets::{self, TargetMap};
use crate::lsp_client::{self, LspClient, LspResolutionResult, ServerHealth, ServerHealthLevel};
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

#[cfg(test)]
#[path = "lsp_resolver/health_gate_tests.rs"]
mod health_gate_tests;

#[cfg(test)]
#[path = "lsp_resolver/outside_targets_tests.rs"]
mod outside_targets_tests;

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
            outside_targets_count: 0,
            elapsed_ms: start.elapsed().as_millis() as u64,
            // No client was ever started — there is nothing to resolve, so
            // there is no server opinion to report either.
            server_health: ServerHealth::not_probed(),
        });
    }

    // fleet-watch#18: definition URIs come back absolute (and on macOS the
    // server may answer under /private/var while the caller passed /var, or
    // vice versa). Canonicalize the root ONCE so uri_to_relative_path can
    // strip it from the canonicalized definition path and recover the same
    // root-relative key the indexer used.
    let canonical_root =
        std::fs::canonicalize(codebase_path).unwrap_or_else(|_| codebase_path.to_path_buf());
    let node_index = build_node_position_index(store)?;
    // Issue #284 (lot 5): discovered once per pass, never per file — the
    // same map every file's outside-target check reads. `TargetMap::Unknown`
    // (no Cargo.toml, `cargo` missing, or a workspace that fails to load —
    // issue #282's own case) attributes nothing, so this never turns a
    // resolver quirk into a false "outside" claim.
    let target_map = cargo_targets::discover(codebase_path);
    let plan = PassPlan {
        codebase_path,
        language,
        deadline: timeout.saturating_sub(PER_REQUEST_TIMEOUT),
        start,
        ctx: SiteContext {
            node_index: &node_index,
            canonical_root: &canonical_root,
        },
        target_map: &target_map,
    };

    let mut client = LspClient::start(cmd, default_args, codebase_path, timeout)?;
    let outcome = resolve_with_client(store, &mut client, &plan, &unresolved);
    // Capture the server's own verdict, and shut it down, regardless of
    // whether the health gate stopped the pass early — `Drop for LspClient`
    // is a safety net, not a substitute for the cooperative handshake.
    let server_health = client.server_health().clone();
    let _ = client.shutdown();
    let pass = outcome?;
    pass.mark_resolved(store)?;
    Ok(pass.into_result(start.elapsed().as_millis() as u64, server_health))
}

/// Everything the per-file loop needs that is fixed for the whole pass,
/// grouped so `drive_pass`/`resolve_with_client` stay within the §4.4
/// parameter cap.
struct PassPlan<'a> {
    codebase_path: &'a Path,
    language: &'a str,
    /// Elapsed time after which no further request is issued.
    deadline: Duration,
    /// When the pass began — `drive_pass`'s per-site budget check measures
    /// elapsed time against this, not against when `resolve_with_client`
    /// itself was entered (the caller may have spent time on `LspClient::start`
    /// first).
    start: Instant,
    ctx: SiteContext<'a>,
    /// Issue #284 (lot 5): the compiled-target surface `drive_pass` consults
    /// per file, before ever issuing a `didOpen`/`definition` request for it.
    target_map: &'a TargetMap,
}

/// Issue #282. Initializes `client` against `plan.codebase_path`, gates on
/// the server's own reported health, and — only if the gate passes — drives
/// the resolution pass.
///
/// Extracted from `resolve_with_lsp` so the health gate is testable against
/// an already-STARTED (but not yet initialized) client, without going
/// through `LspClient::start`'s command allowlist — see
/// `lsp_resolver/health_gate_tests.rs`.
///
/// `Err` here means: the server itself reported it could not load the
/// project (`health: "error"`, sonde B — `lsp_client::health` module
/// header). No `textDocument/definition` request is ever issued in that
/// case — every one of them would answer `[]`, indistinguishable from 634
/// individually "not found" call sites (issue #282's measured symptom).
fn resolve_with_client(
    store: &GraphStore,
    client: &mut LspClient,
    plan: &PassPlan<'_>,
    unresolved: &[UnresolvedCallSite],
) -> Result<LspPass, String> {
    client.initialize(plan.codebase_path)?;
    if let Some(err) = health_gate_error(client.server_health()) {
        return Err(err);
    }
    Ok(drive_pass(store, client, plan, unresolved, plan.start))
}

/// `Some(error)` when the server's last-observed health is `Error` — the
/// §1.2 arbitrated decision: `error` fails the phase outright before any
/// resolution request, `warning` does not (rust-analyzer emits `warning` for
/// causes that still resolve nothing but are not this specific failure —
/// sonde E, `lsp_client::health` module header — and is instead surfaced via
/// `completed_unresolved`, `lsp_outcome::completed_state`).
///
/// The remedy text names the fix directly (§1.2 decision (b) rationale):
/// there is nothing for this tool to correct — a crate excluded from its
/// parent workspace's `members` is a state of the analyzed repo, and the
/// honest response is to say so loudly and name the cargo-level fix.
fn health_gate_error(health: &ServerHealth) -> Option<String> {
    if health.level != ServerHealthLevel::Error {
        return None;
    }
    let message = health
        .message
        .clone()
        .unwrap_or_else(|| "no message from server".to_string());
    Some(format!(
        "lsp_workspace_load_failed: {message}; the language server loaded no crate graph, so \
         every definition request would answer []. Fix: add the package to the parent \
         workspace's members, or analyze the workspace root"
    ))
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
        // Issue #284 (lot 5): a file outside every compiled Cargo target is
        // never in rust-analyzer's crate graph — every `textDocument/definition`
        // request against it would answer `[]` (probe C,
        // `tasks/plan-issues-282-283-284.md` §0.2), indistinguishable from a
        // real "not found". Attribute and move on without issuing the
        // `didOpen`/`definition` round trip that would only confirm the same
        // thing 634 times over on the dy-wcet corpus.
        if plan.target_map.is_outside_targets(Path::new(file_path)) {
            pass.mark_outside_targets(sites);
            continue;
        }
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
            let (line, col) = site.lsp_position();
            pass.record(
                store,
                site,
                client.get_definition(&file_uri, line, col),
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
