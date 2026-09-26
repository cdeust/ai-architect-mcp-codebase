// tool_schemas — MCP tool JSON Schema definitions.
//
// Pure data: every function returns a `serde_json::Value` containing the
// JSON-RPC tool schema. No logic, no I/O. Extracted from main.rs when
// tools_list() exceeded 200 LOC (NOTES.md growth rule).

use serde_json::{json, Value};

#[path = "tool_schemas_shared_params.rs"]
mod shared_params;

#[path = "tool_schemas_index_codebase.rs"]
mod index_codebase;
use index_codebase::index_codebase_schema;

#[path = "tool_schemas_analyze_codebase.rs"]
mod analyze_codebase;
use analyze_codebase::analyze_codebase_schema;

#[path = "tool_schemas_findings.rs"]
mod findings;
use findings::{
    abort_verification_schema, append_clarification_schema, extract_finding_schema,
    finalize_verification_schema, refine_finding_schema, start_verification_schema,
};

#[path = "tool_schemas_graph_core.rs"]
mod graph_core;
use graph_core::{
    cluster_graph_schema, get_processes_schema, get_symbol_schema, index_history_schema,
    index_status_schema, ingest_traces_schema, query_graph_schema, resolve_graph_schema,
};

#[path = "tool_schemas_graph_analysis.rs"]
mod graph_analysis;
use graph_analysis::{
    detect_changes_schema, get_context_schema, get_impact_schema, lsp_resolve_schema,
    search_codebase_schema,
};

#[path = "tool_schemas_prd_security.rs"]
mod prd_security;
use prd_security::{
    check_security_gates_schema, prepare_prd_input_schema, validate_prd_against_graph_schema,
    verify_semantic_diff_schema,
};

/// What a refused call has already written (issue #363).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HeldGraphEffect {
    /// The graph is opened once, before any write: a refused call wrote nothing.
    SingleOpen,
    /// The graph is opened again at a later stage: a refusal there comes after
    /// earlier stages wrote. The text says which.
    Staged(&'static str),
}

/// Tools whose call opens, rewrites, removes or imports a graph through
/// `GraphStore::open_or_create` or the removal and import helpers, and so can
/// be refused by the handle release of issue #352, with what a refused call
/// has already written. The read tools reuse the cache's handle and never
/// release, so they cannot return these codes. The list is measured, not kept
/// by hand: `held_graph_tools_tests` holds a handle on a real graph, calls
/// every graph tool and requires the refusing set to equal this list.
pub(crate) const HELD_GRAPH_TOOLS: &[(&str, HeldGraphEffect)] = &[
    ("index_codebase", HeldGraphEffect::SingleOpen),
    ("ingest_traces", HeldGraphEffect::SingleOpen),
    (
        "analyze_codebase",
        HeldGraphEffect::Staged(
            " A refusal before indexing writes nothing. A refusal when the resolve stage opens \
            the graph comes after the index wrote it: the graph is indexed but unresolved. A \
            refusal at the final LSP durability check comes after every stage wrote. Rerun \
            analyze_codebase once the other request has returned.",
        ),
    ),
    ("resolve_graph", HeldGraphEffect::SingleOpen),
    ("cluster_graph", HeldGraphEffect::SingleOpen),
    (
        "lsp_resolve",
        HeldGraphEffect::Staged(
            " A refusal when the pass opens the graph writes nothing. A refusal when it reopens \
            the graph to count its rows comes after the pass wrote them, so whether they are \
            durable is unknown: rerun lsp_resolve once the other request has returned.",
        ),
    ),
    ("index_history", HeldGraphEffect::SingleOpen),
    ("prepare_prd_input", HeldGraphEffect::SingleOpen),
    ("verify_semantic_diff", HeldGraphEffect::SingleOpen),
];

/// The part of the note every refusable tool shares.
const HELD_GRAPH_CODES: &str = " GRAPH HANDLE ERRORS (issue #352): the call fails with \
    graph_handle_in_use when a running request still holds the graph's read handle, and with \
    graph_cache_busy when the handle cache is being modified; the code opens the error message.";

/// The note of a tool that opens the graph once, before any write.
const HELD_GRAPH_SINGLE_OPEN: &str = " The graph is opened once, before anything is written, \
    so a refused call wrote nothing. Retry once the other request has returned.";

/// Restarting the server releases every handle.
const HELD_GRAPH_RESTART: &str = " Restarting the server clears both codes.";

/// Returns the full `tools/list` response payload.
pub fn tools_list() -> Value {
    let mut tools = vec![
        health_check_schema(),
        extract_finding_schema(),
        refine_finding_schema(),
        start_verification_schema(),
        append_clarification_schema(),
        finalize_verification_schema(),
        abort_verification_schema(),
        index_codebase_schema(),
        index_status_schema(),
        ingest_traces_schema(),
        query_graph_schema(),
        get_symbol_schema(),
        resolve_graph_schema(),
        cluster_graph_schema(),
        get_processes_schema(),
        get_impact_schema(),
        index_history_schema(),
        search_codebase_schema(),
        get_context_schema(),
        analyze_codebase_schema(),
        detect_changes_schema(),
        lsp_resolve_schema(),
        prepare_prd_input_schema(),
        validate_prd_against_graph_schema(),
        check_security_gates_schema(),
        verify_semantic_diff_schema(),
    ];
    for tool in &mut tools {
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
        let Some(&(_, effect)) = HELD_GRAPH_TOOLS.iter().find(|(tool, _)| *tool == name) else {
            continue;
        };
        let effect = match effect {
            HeldGraphEffect::SingleOpen => HELD_GRAPH_SINGLE_OPEN,
            HeldGraphEffect::Staged(text) => text,
        };
        if let Some(Value::String(description)) = tool.get_mut("description") {
            description.push_str(HELD_GRAPH_CODES);
            description.push_str(effect);
            description.push_str(HELD_GRAPH_RESTART);
        }
    }
    json!({ "tools": tools })
}

/// The canonical one-line summary of a tool: the first sentence of its
/// `tools_list()` description. This is the SINGLE source of truth for what a
/// tool does — the prompts layer (`mcp_prompts`) composes workflow text from
/// this rather than hand-copying descriptions, so adding or renaming a tool
/// cannot leave two descriptions that disagree (§1.2 / issue #65 criterion 3).
///
/// Precondition: `name` is a candidate tool name (any string).
/// Postcondition: returns `Some(first_sentence)` iff a tool with that exact
/// name is registered in `tools_list()`; `None` otherwise. The returned string
/// is the description text up to and including the first ". " boundary (or the
/// whole description when it contains no such boundary).
pub fn tool_summary(name: &str) -> Option<String> {
    let payload = tools_list();
    let tools = payload.get("tools").and_then(Value::as_array)?;
    let description = tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|tool| tool.get("description").and_then(Value::as_str))?;
    // First sentence: text up to and including the first ". " boundary. AP's
    // descriptions open with a stage tag then a sentence (e.g. "Stage 3a — ….")
    // which is exactly the orientation a workflow prompt needs.
    let first = match description.split_once(". ") {
        Some((head, _)) => format!("{head}."),
        None => description.to_string(),
    };
    Some(first)
}

fn health_check_schema() -> Value {
    json!({
        "name": "health_check",
        "description": "Stage 0 — Handshake and liveness probe. CALL THIS FIRST, before any other tool, to confirm the MCP is live and to learn which tool profile is active (core vs full) so you know which tools are registered this session. Returns: server identity + version, the MCP protocol version, and the count of registered tools. Needs no graph and no arguments. This is a cheap read-only sanity check, not a data tool — for a graph's node/edge counts and indexing coverage use index_status instead.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    })
}

#[cfg(test)]
mod held_graph_tests {
    use super::*;

    fn description_of(name: &str) -> String {
        let payload = tools_list();
        payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not a registered tool"))["description"]
            .as_str()
            .expect("description")
            .to_string()
    }

    // The two codes reach a client as the prefix of a tool's failure message
    // (issue #352); a client can only act on them if the schema names them.
    #[test]
    fn every_tool_that_can_be_refused_documents_both_codes() {
        for (name, _) in HELD_GRAPH_TOOLS {
            let description = description_of(name);
            for code in ["graph_handle_in_use", "graph_cache_busy"] {
                assert!(
                    description.contains(code),
                    "{name} does not document {code}"
                );
            }
        }
    }

    // Issue #363: "nothing was written" is true only for a tool that opens the
    // graph once, before any write. The two tools that open it again at a later
    // stage say what the earlier stages already wrote.
    #[test]
    fn only_single_open_tools_claim_that_nothing_was_written() {
        for (name, effect) in HELD_GRAPH_TOOLS {
            let description = description_of(name);
            let staged = matches!(effect, HeldGraphEffect::Staged(_));
            assert_eq!(
                description.contains("wrote nothing"),
                !staged,
                "{name}: the note must match how many times the tool opens the graph"
            );
            if staged {
                assert!(description.contains("comes after"), "{name}: {description}");
            }
        }
    }

    #[test]
    fn a_read_tool_does_not_claim_the_write_refusal() {
        for name in ["query_graph", "get_symbol", "get_impact", "detect_changes"] {
            assert!(
                !description_of(name).contains("graph_handle_in_use"),
                "{name} reuses the cache handle and cannot be refused"
            );
        }
    }

    #[test]
    fn lsp_resolve_documents_its_durability_code_too() {
        assert!(description_of("lsp_resolve").contains("lsp_rows_not_durable"));
    }
}
