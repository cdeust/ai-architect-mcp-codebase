//! GRAPH condition — answers each question with exactly one AP graph tool, on
//! the same code paths the MCP tools run. No file reads.
//!
//! `tool_calls` counts the single conceptual MCP tool invocation per question
//! (query_graph / get_impact / get_processes / search_codebase). Internal
//! name→qualified-name resolution is part of that tool, not a separate call.

use super::{file_of_qn, Answer};
use crate::questions::{GraphOp, Question};
use ai_architect_mcp::clustering::{get_impact, get_processes};
use ai_architect_mcp::graph_store::{cypher_str, GraphStore};
use ai_architect_mcp::search::{self, SearchOptions};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// A returned graph node, reduced to what the answer needs.
struct NodeRef {
    qn: String,
    label: String,
}

/// Run the GRAPH condition for one question.
pub fn run(q: &Question, store: &GraphStore, graph_path: &Path) -> Answer {
    let nodes = match q.graph_op {
        GraphOp::Define => op_define(store, &q.target),
        GraphOp::ImpactCallers => op_impact_callers(store, &q.target),
        GraphOp::ImpactUsers => op_impact_users(store, &q.target),
        GraphOp::ProcessesMain => op_processes_main(store),
        GraphOp::Search => op_search(store, graph_path, &q.target),
    };
    finalize(&q.graph_op, nodes)
}

/// D1 — query_graph exact-name lookup. Returns only nodes whose `name` matches
/// exactly, so substring-colliding distractors (process_orders) are excluded.
fn op_define(store: &GraphStore, name: &str) -> Vec<NodeRef> {
    let cypher = format!(
        "MATCH (f:Function) WHERE f.name = {} RETURN f.qualified_name",
        cypher_str(name)
    );
    query_qns(store, &cypher, "Function")
}

/// D2 — get_impact reverse-Calls.
fn op_impact_callers(store: &GraphStore, name: &str) -> Vec<NodeRef> {
    let Some(qn) = resolve_qn(store, name, &["Function", "Method"]) else {
        return Vec::new();
    };
    let Ok(impact) = get_impact(store, &qn) else {
        return Vec::new();
    };
    impact
        .callers
        .into_iter()
        .map(|n| NodeRef {
            qn: n.qualified_name,
            label: n.label,
        })
        .collect()
}

/// D4 — get_impact users + importers of a type.
fn op_impact_users(store: &GraphStore, name: &str) -> Vec<NodeRef> {
    let labels = ["Struct", "Class", "Interface", "Enum", "Type"];
    let Some(qn) = resolve_qn(store, name, &labels) else {
        return Vec::new();
    };
    let Ok(impact) = get_impact(store, &qn) else {
        return Vec::new();
    };
    impact
        .users
        .into_iter()
        .chain(impact.importers)
        .map(|n| NodeRef {
            qn: n.qualified_name,
            label: n.label,
        })
        .collect()
}

/// D3 — get_processes filtered to program entry points (kind == "main").
fn op_processes_main(store: &GraphStore) -> Vec<NodeRef> {
    get_processes(store)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.entry_kind == "main")
        .map(|p| NodeRef {
            qn: p.entry_point,
            label: "Process".to_string(),
        })
        .collect()
}

/// D5 — search_codebase (hybrid/substring, the real tool path).
fn op_search(store: &GraphStore, graph_path: &Path, keyword: &str) -> Vec<NodeRef> {
    let index_dir = search::resolve_search_index_dir(graph_path);
    let opts = SearchOptions {
        limit: 50,
        label_filter: None,
        min_score: 0.01,
    };
    search::search_graph(store, keyword, &opts, index_dir.as_deref())
        .unwrap_or_default()
        .into_iter()
        .map(|r| NodeRef {
            qn: r.qualified_name,
            label: r.label,
        })
        .collect()
}

/// Resolve a bare symbol name to its qualified name, trying each label in order.
/// Returns the first match (the corpus has one definition per queried name).
fn resolve_qn(store: &GraphStore, name: &str, labels: &[&str]) -> Option<String> {
    for lbl in labels {
        let cypher = format!(
            "MATCH (n:{lbl}) WHERE n.name = {} RETURN n.qualified_name",
            cypher_str(name)
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if let Some(qn) = row.first() {
                    return Some(qn.clone());
                }
            }
        }
    }
    None
}

/// Run a Cypher query returning a single qualified-name column into NodeRefs.
fn query_qns(store: &GraphStore, cypher: &str, label: &str) -> Vec<NodeRef> {
    let Ok(qr) = store.execute_query(cypher) else {
        return Vec::new();
    };
    qr.rows
        .into_iter()
        .filter_map(|row| row.into_iter().next())
        .map(|qn| NodeRef {
            qn,
            label: label.to_string(),
        })
        .collect()
}

/// Build the [`Answer`]: file set + compact serialization (the token proxy) +
/// one tool call.
fn finalize(op: &GraphOp, nodes: Vec<NodeRef>) -> Answer {
    let files: BTreeSet<String> = nodes.iter().map(|n| file_of_qn(&n.qn)).collect();
    let node_json: Vec<Value> = nodes
        .iter()
        .map(|n| json!({ "qn": n.qn, "file": file_of_qn(&n.qn), "label": n.label }))
        .collect();
    let envelope = json!({
        "tool": tool_name(op),
        "count": nodes.len(),
        "nodes": node_json,
    });
    let text = serde_json::to_string(&envelope).unwrap_or_default();
    let chars = text.len();
    Answer {
        files,
        chars,
        tool_calls: 1,
        text,
    }
}

/// MCP tool name for an op (used in the serialized envelope).
fn tool_name(op: &GraphOp) -> &'static str {
    match op {
        GraphOp::Define => "query_graph",
        GraphOp::ImpactCallers | GraphOp::ImpactUsers => "get_impact",
        GraphOp::ProcessesMain => "get_processes",
        GraphOp::Search => "search_codebase",
    }
}
