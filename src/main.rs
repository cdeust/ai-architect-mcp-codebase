// AI Architect Codebase — graph-grounded code intelligence over MCP.
//
// Transport: stdio JSON-RPC 2.0, hand-rolled (no MCP SDK — we own the protocol
// wire layer so the agents know exactly what's happening).
//
// Today:
//   Stage 0 — health_check         (handshake + server state)
//   Stage 1 — extract_finding      (finding → extracted artifact + index entry)
//   Stage 1 — refine_finding       (orchestrator refinement → refined artifact)
//   Stage 2 — start_verification   (create clarification session)
//   Stage 2 — append_clarification (append one turn, advance state machine)
//   Stage 2 — finalize_verification(compute digest, write verified receipt)
//   Stage 2 — abort_verification   (kill a non-terminal session)
//
// Each future stage lands as one more entry in `tools_list()` + one more arm
// in `handle_tool_call()`. No pre-scaffolding of layers or helpers.
//
// Reference implementation (to read, not copy): /Users/cdeust/Developments/ai-architect

mod ambiguity_policy;
mod artifact;
mod bridge;
mod call_evidence;
mod cli;
mod clustering;
mod cochange;
mod epistemic;
mod git_diff;
mod graph_cache;
mod graph_store;
mod history;
mod hook_augment;
mod host_install;
mod indexer;
mod language_provider;
mod lsp_client;
mod lsp_resolver;
mod macro_expansion;
mod mcp_prompts;
mod parser;
mod prd_input;
mod prd_validator;
mod resolver;
mod resolver_layers;
mod response_budget;
mod search;
mod security_gates;
mod semantic_diff;
mod stdlib_index;
#[cfg(test)]
mod test_support;
mod token_surface;
mod tool_profile;
mod tool_schemas;
mod write_diagnostics;

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::io::{self, BufRead, Write};
use std::process;
use tool_profile::ToolProfile;

// ---------------------------------------------------------------------------
// Protocol / server identity
// ---------------------------------------------------------------------------

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "ai-architect";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

// `initialize.instructions` teaches the connecting LLM the active profile's
// intended workflow. The text VARIES by profile (issue #65 criterion 5): a
// `core` client is told what `core` is for, not handed the full-pipeline map.
// source: tool_profile.rs — CORE_TOOL_NAMES is the read-only code-intelligence
// surface; Full is the whole finding→verification→PRD→gates pipeline.
const CORE_INSTRUCTIONS: &str =
    "ai-architect, 'core' profile — the read-only code-intelligence surface (8 tools). \
     Start with health_check to confirm liveness and the active profile, then analyze_codebase \
     on a repo to index + resolve + cluster the graph in one call. Explore with search_codebase \
     (keyword → ranked symbols), get_context / get_symbol (a symbol's neighborhood), get_impact \
     (reverse-dependency blast radius), detect_changes (a diff's blast radius), and query_graph \
     (arbitrary read-only Cypher). Absence from the graph is NOT proof of absence — a file may \
     be parse-incomplete; verify negatives with query_graph(graph=\"missed\"). Two guided \
     workflows are published via prompts/list: explore_codebase and review_change_impact. The \
     internal finding→verification→PRD pipeline stages are hidden in this profile; restart with \
     --profile full to expose them.";
const FULL_INSTRUCTIONS: &str =
    "ai-architect, 'full' profile — the entire pipeline (every tool; the default). The tools are \
     strongly ordered and the order is not guessable; a caller that gets it wrong gets an empty \
     or misleading result rather than an error. Stages: (1) findings — extract_finding → \
     refine_finding; (2) verification — start_verification → append_clarification → \
     finalize_verification; (3) code intelligence — index_codebase → resolve_graph → \
     cluster_graph (or analyze_codebase for all three), then search_codebase / get_context / \
     get_symbol / get_impact / detect_changes / query_graph; (4/6) PRD grounding — \
     prepare_prd_input → validate_prd_against_graph; (8/9) gates — check_security_gates, \
     verify_semantic_diff. Absence from the graph is NOT proof of absence — verify negatives \
     with query_graph(graph=\"missed\"). Guided workflows are published via prompts/list: \
     explore_codebase, review_change_impact, verify_finding, ground_prd. For the read-only \
     code-intelligence subset only, restart with --profile core.";

/// The `initialize.instructions` string for `profile` (issue #65 criterion 5).
fn server_instructions(profile: ToolProfile) -> &'static str {
    match profile {
        ToolProfile::Core => CORE_INSTRUCTIONS,
        ToolProfile::Full => FULL_INSTRUCTIONS,
    }
}

// ---------------------------------------------------------------------------
// Wire types (MCP JSON-RPC layer)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Request {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn write_message(msg: Value) {
    let line = msg.to_string();
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{}", line);
    let _ = lock.flush();
}

fn send_response(id: Value, result: Value) {
    write_message(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }));
}

fn send_error(id: Value, code: i32, message: &str) {
    write_message(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }));
}

// ---------------------------------------------------------------------------
// Tool registry — one entry per pipeline stage.
// ---------------------------------------------------------------------------

fn tools_list(profile: ToolProfile) -> Value {
    profile.filter_tools_list(tool_schemas::tools_list())
}

mod analyze_handlers;
mod finding_extract;
mod finding_index;
mod finding_refine;
mod handler_util;
mod history_handlers;
mod indexing_handlers;
mod prd_handlers;
mod process_impact_handlers;
mod query_handlers;
mod search_context_handlers;
mod symbol_handlers;
mod verification_core;
mod verification_ops;

pub(crate) use analyze_handlers::*;
pub(crate) use finding_extract::*;
pub(crate) use finding_refine::*;
#[cfg(test)]
pub(crate) use history_handlers::*;
pub(crate) use indexing_handlers::*;
pub(crate) use prd_handlers::*;
pub(crate) use process_impact_handlers::*;
pub(crate) use query_handlers::*;
pub(crate) use search_context_handlers::*;
pub(crate) use symbol_handlers::*;
pub(crate) use verification_ops::*;

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

fn handle_tool_call(params: &Value, profile: ToolProfile) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    // A tool the active profile does not register must behave exactly like
    // a tool that does not exist — the profile IS the registry.
    if !profile.allows(name) {
        return json!({
            "isError": true,
            "content": [{
                "type": "text",
                "text": format!(
                    "Unknown tool: {} (not registered under the '{}' profile; \
                     restart with --profile full to expose every tool)",
                    name,
                    profile.name()
                )
            }]
        });
    }

    let payload = match name {
        "health_check" => {
            // source: C-correctness bug 3 — the count was a hardcoded `19`
            // that silently lied if a new tool was added without bumping it.
            // Derive from tools_list() so the count can never drift.
            let tools_count = tools_list(profile)
                .get("tools")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            json!({
                "stage": 0,
                "name": "health_check",
                "status": "ok",
                "server": SERVER_NAME,
                "version": SERVER_VERSION,
                "protocol": PROTOCOL_VERSION,
                // Kept for back-compat with existing clients that parse
                // `stages_registered`. Both now reflect the live tool count.
                "stages_registered": tools_count,
                "tools_count": tools_count,
            })
        }
        "extract_finding" => run_extract_finding(&arguments),
        "refine_finding" => run_refine_finding(&arguments),
        "start_verification" => run_start_verification(&arguments),
        "append_clarification" => run_append_clarification(&arguments),
        "finalize_verification" => run_finalize_verification(&arguments),
        "abort_verification" => run_abort_verification(&arguments),
        "index_codebase" => run_index_codebase(&arguments),
        "index_status" => run_index_status(&arguments),
        "ingest_traces" => run_ingest_traces(&arguments),
        "query_graph" => run_query_graph(&arguments),
        "get_symbol" => run_get_symbol(&arguments),
        "resolve_graph" => run_resolve_graph(&arguments),
        "cluster_graph" => run_cluster_graph(&arguments),
        "get_processes" => run_get_processes(&arguments),
        "get_impact" => run_get_impact(&arguments),
        "index_history" => run_index_history(&arguments),
        "search_codebase" => run_search_codebase(&arguments),
        "get_context" => run_get_context(&arguments),
        "analyze_codebase" => run_analyze_codebase(&arguments),
        "detect_changes" => run_detect_changes(&arguments),
        "lsp_resolve" => run_lsp_resolve(&arguments),
        "prepare_prd_input" => run_prepare_prd_input(&arguments),
        "validate_prd_against_graph" => run_validate_prd_against_graph(&arguments),
        "check_security_gates" => run_check_security_gates(&arguments),
        "verify_semantic_diff" => run_verify_semantic_diff(&arguments),
        other => {
            return json!({
                "isError": true,
                "content": [{
                    "type": "text",
                    "text": format!("Unknown tool: {}", other)
                }]
            });
        }
    };

    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
        }]
    })
}

// ---------------------------------------------------------------------------
// Request dispatch
// ---------------------------------------------------------------------------

fn handle_request(req: Request, profile: ToolProfile) {
    let id = req.id.clone().unwrap_or(Value::Null);

    match req.method.as_str() {
        "initialize" => send_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                // Declare prompts alongside tools. Resources are intentionally
                // NOT declared — see resources/list below: some clients probe
                // resources regardless of declared capabilities.
                "capabilities": {
                    "tools": {},
                    "prompts": { "listChanged": false }
                },
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                // Per-profile workflow guidance (issue #65 criterion 5).
                "instructions": server_instructions(profile)
            }),
        ),
        "notifications/initialized" => {
            // JSON-RPC notification: no response
        }
        "tools/list" => send_response(id, tools_list(profile)),
        "tools/call" => send_response(id, handle_tool_call(&req.params, profile)),
        "prompts/list" => send_response(id, mcp_prompts::prompts_list(profile)),
        "prompts/get" => match mcp_prompts::prompt_get(&req.params, profile) {
            Ok(result) => send_response(id, result),
            Err((code, message)) => send_error(id, code, &message),
        },
        // Empty-but-present resource endpoints. This server exposes no
        // resources, but some MCP clients probe these on connect regardless of
        // declared capabilities and surface the resulting -32601 as a failed
        // connection (CBM upstream #958). An empty list is interoperable.
        // source: DeusData/codebase-memory-mcp src/mcp/mcp.c:10810-10816 (#958).
        "resources/list" => send_response(id, json!({ "resources": [] })),
        "resources/templates/list" => send_response(id, json!({ "resourceTemplates": [] })),
        other => send_error(id, -32601, &format!("Method not found: {}", other)),
    }
}

// ---------------------------------------------------------------------------
// stdio loop
// ---------------------------------------------------------------------------

fn main() {
    // Read-once-at-startup configuration: `--profile` flag beats `AP_PROFILE`
    // env var; absent both, every tool is registered (see tool_profile.rs).
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Issue #59 subcommands run and exit BEFORE the MCP stdio server starts.
    match args.first().map(String::as_str) {
        Some("install") => process::exit(cli::run_install(&args[1..])),
        Some("uninstall") => process::exit(cli::run_uninstall(&args[1..])),
        Some("hook-augment") => process::exit(cli::run_hook_augment()),
        _ => {}
    }

    let env_profile = std::env::var(tool_profile::PROFILE_ENV_VAR).ok();
    let profile = match ToolProfile::resolve(&args, env_profile.as_deref()) {
        Ok(profile) => profile,
        Err(message) => {
            eprintln!("[ai-architect-mcp-codebase] {}", message);
            // source: POSIX utility convention (e.g. grep(1)) — exit status 2
            // signals a usage error, distinct from runtime failure (1).
            process::exit(2);
        }
    };

    eprintln!(
        "[ai-architect-mcp-codebase] stage 0-3d up (Rust {}, profile '{}')",
        SERVER_VERSION,
        profile.name()
    );

    let stdin = io::stdin();
    let handle = stdin.lock();
    for line in handle.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[ai-architect-mcp-codebase] stdin error: {}", e);
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => handle_request(req, profile),
            Err(e) => eprintln!("[ai-architect-mcp-codebase] parse error: {}", e),
        }
    }
}

#[cfg(test)]
#[path = "artifact_bootstrap_tests.rs"]
mod artifact_bootstrap_tests;
#[cfg(test)]
#[path = "coverage_tools_tests.rs"]
mod coverage_tools_tests;
#[cfg(test)]
#[path = "pagination_tests.rs"]
mod pagination_tests;
#[cfg(test)]
#[path = "temporal_runtime_tests.rs"]
mod temporal_runtime_tests;
#[cfg(test)]
#[path = "token_surface_tools_tests.rs"]
mod token_surface_tools_tests;
