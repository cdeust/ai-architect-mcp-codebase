//! Stage 3a-3c — `get_symbol` / `resolve_graph` / `cluster_graph` handler
//! logic, plus the relationship-table triples used by symbol edge lookups.
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use crate::bridge;
use crate::search;
use serde_json::{json, Value};
use std::path::Path;

use crate::clustering;
use crate::graph_cache;
use crate::graph_store;
use crate::resolver;

// ---------------------------------------------------------------------------
// Stage 3a — get_symbol
// ---------------------------------------------------------------------------

pub(crate) fn run_get_symbol(arguments: &Value) -> Value {
    match do_get_symbol(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "symbol_lookup_failed", "message": msg
        }),
    }
}

pub(crate) fn do_get_symbol(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let qn = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'qualified_name'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;

    // source: C-correctness bug 2 — three-layer lookup (exact → strip-path →
    // fuzzy). Resolves the natural `src/main.rs::foo` form to the stored
    // `main.rs::foo` and returns did_you_mean suggestions otherwise.
    let resolved_qn = match search::resolve_qualified_name(&store, qn) {
        Ok(q) => q,
        Err(nf) => {
            // Cross-repo bridge: a symbol absent locally may be DEFINED in a
            // sibling repo (the dangling reference the resolver marks external).
            // Consult siblings before declaring it not-found. Absent the arg
            // this is a no-op and the original error surfaces unchanged.
            // source: cross-repo bridge spec (bridge module).
            let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
            let foreign = if siblings.is_empty() {
                Vec::new()
            } else {
                bridge::resolve_definition(&siblings, qn)
            };
            if !foreign.is_empty() {
                return Ok(json!({
                    "stage": 3,
                    "status": "ok",
                    "tool": "get_symbol",
                    "resolved_in": "sibling",
                    "message": format!(
                        "'{qn}' is not defined in this graph but is defined in \
                         {} sibling location(s)", foreign.len()),
                    "foreign_definitions": foreign.iter().map(|f| f.to_json()).collect::<Vec<_>>(),
                    "did_you_mean": nf.did_you_mean,
                    "next_steps": [
                        "re-query the owning repo: get_symbol with graph_path set \
                         to the `repo` of a foreign definition".to_string(),
                    ],
                }));
            }
            return Ok(json!({
                "stage": 3,
                "status": "error",
                "reason": "symbol_not_found",
                "message": format!("not found: {}", nf.input),
                "did_you_mean": nf.did_you_mean,
            }));
        }
    };

    // source: M1 fix — centralized cypher_str escapes both `\` and `'`.
    // Returns the string already wrapped in single quotes.
    let escaped = graph_store::cypher_str(&resolved_qn);

    let node = find_symbol_node(&store, &escaped)?;
    let edges_out = find_symbol_edges_out(&store, &escaped)?;
    let edges_in = find_symbol_edges_in(&store, &escaped)?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "get_symbol",
        "node": node,
        "edges_out": edges_out,
        "edges_in": edges_in,
        "next_steps": [
            format!("see relationship context: get_context on '{resolved_qn}'"),
            format!("trace blast radius before changing it: get_impact on '{resolved_qn}'"),
        ],
    }))
}

/// Searches all node tables for a node matching by qualified_name or id.
/// `lit` must be a Cypher-quoted literal produced by `graph_store::cypher_str`.
pub(crate) fn find_symbol_node(
    store: &graph_store::GraphStore,
    lit: &str,
) -> Result<Value, String> {
    let labels = [
        "Function",
        "Method",
        "Struct",
        "Enum",
        "Trait",
        "Variant",
        "Module",
        "Constant",
        "TypeAlias",
        "Field",
        "Import",
        "File",
        "Directory",
        "CallSite",
    ];
    for label in labels {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = {lit} \
             OR n.id = {lit} RETURN n"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if !qr.rows.is_empty() {
                return Ok(json!({
                    "label": label,
                    "data": qr.rows[0][0],
                }));
            }
        }
    }
    Ok(Value::Null)
}

/// Queries outgoing edges across all relationship tables.
pub(crate) fn find_symbol_edges_out(
    store: &graph_store::GraphStore,
    lit: &str,
) -> Result<Vec<Value>, String> {
    let mut edges = Vec::new();
    for (rel, from_label, to_label) in rel_table_triples() {
        let cypher = format!(
            "MATCH (a:{from_label})-[r:{rel}]->(b:{to_label}) \
             WHERE a.qualified_name = {lit} OR a.id = {lit} \
             RETURN '{rel}' AS rel_type, b.id AS target_id"
        );
        collect_edge_rows(store, &cypher, &mut edges);
    }
    Ok(edges)
}

/// Queries incoming edges across all relationship tables.
pub(crate) fn find_symbol_edges_in(
    store: &graph_store::GraphStore,
    lit: &str,
) -> Result<Vec<Value>, String> {
    let mut edges = Vec::new();
    for (rel, from_label, to_label) in rel_table_triples() {
        let cypher = format!(
            "MATCH (a:{from_label})-[r:{rel}]->(b:{to_label}) \
             WHERE b.qualified_name = {lit} OR b.id = {lit} \
             RETURN '{rel}' AS rel_type, a.id AS source_id"
        );
        collect_edge_rows(store, &cypher, &mut edges);
    }
    Ok(edges)
}

pub(crate) fn collect_edge_rows(
    store: &graph_store::GraphStore,
    cypher: &str,
    edges: &mut Vec<Value>,
) {
    if let Ok(qr) = store.execute_query(cypher) {
        for row in &qr.rows {
            if row.len() >= 2 {
                edges.push(json!({"rel": row[0], "id": row[1]}));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stage 3b — resolve_graph
// ---------------------------------------------------------------------------

pub(crate) fn run_resolve_graph(arguments: &Value) -> Value {
    match do_resolve_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "resolve_failed", "message": msg
        }),
    }
}

pub(crate) fn do_resolve_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let result = resolver::resolve_graph(&store)?;
    let rate = if result.total_refs > 0 {
        result.total_edges as f64 / result.total_refs as f64
    } else {
        0.0
    };

    let mut out = json!({
        "stage": 3,
        "status": "ok",
        "tool": "resolve_graph",
        "imports_resolved": result.imports_resolved,
        "calls_resolved": result.calls_resolved,
        "implements_resolved": result.impls_resolved,
        "extends_resolved": result.extends_resolved,
        "uses_resolved": result.uses_resolved,
        "total_edges": result.total_edges,
        "total_refs": result.total_refs,
        "resolution_rate": format!("{:.2}", rate),
        "unresolved_count": result.unresolved.len(),
        "elapsed_ms": result.elapsed_ms,
    });

    // Cross-repo bridge: report how many of the locally-unresolved references
    // a sibling repo can define — i.e. how much of the dangling set is a
    // cross-service edge rather than a true external/third-party dependency.
    // Absent the arg this is a no-op. source: cross-repo bridge spec.
    let siblings = bridge::SiblingGraphs::from_arg(arguments, graph_path);
    if !siblings.is_empty() {
        // Exclude refs already classified external (stdlib/framework/
        // third-party root, e.g. androidx.*, retrofit2.*) from the
        // candidate pool: those are known-external by construction, not
        // potential cross-repo bridges, regardless of whether the
        // classifying provider's prefix list caught them. This filter and
        // the prefix-list widening in language_provider.rs are independent
        // defenses — issue #31.
        let targets: Vec<String> = result
            .unresolved
            .iter()
            .filter(|u| u.reason != resolver::EXTERNAL_UNRESOLVED_REASON)
            .map(|u| u.target_text.clone())
            .collect();
        let (resolvable, sample) = bridge::count_cross_repo_resolvable(&siblings, &targets);
        out["cross_repo_resolvable"] = json!(resolvable);
        out["cross_repo_sample"] = json!(sample);
        if !siblings.skipped.is_empty() {
            out["sibling_graphs_skipped"] = json!(siblings.skipped);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Stage 3c — cluster_graph
// ---------------------------------------------------------------------------

pub(crate) fn run_cluster_graph(arguments: &Value) -> Value {
    match do_cluster_graph(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "cluster_failed", "message": msg
        }),
    }
}

pub(crate) fn do_cluster_graph(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let gamma = args
        .get("resolution_param")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let result = clustering::cluster_graph(&store, gamma)?;
    let memberships = clustering::collect_cluster_memberships(&store)?;

    let clusters: Vec<Value> = memberships
        .entries
        .iter()
        .map(|m| {
            json!({
                "qualified_name": m.qualified_name,
                "community_id": m.community_id,
                "qn": m.qualified_name,
                "cluster_id": m.cluster_id,
            })
        })
        .collect();

    let mut body = json!({
        "stage": 3,
        "status": "ok",
        "tool": "cluster_graph",
        "community_count": result.communities,
        "modularity": format!("{:.6}", result.modularity),
        "process_count": result.processes,
        "elapsed_ms": result.elapsed_ms,
        "clusters": clusters,
        "total_memberships": memberships.total,
    });
    if let Some(n) = memberships.truncated_at {
        body["clusters_truncated_at"] = json!(n);
    }
    Ok(body)
}

/// All known relationship tables as (name, from_label, to_label).
/// Source: graph_store.rs REL_TABLES (mirrored here because the const is private).
pub(crate) fn rel_table_triples() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        // 3a tables
        ("Contains_Dir_File", "Directory", "File"),
        ("Contains_Dir_Dir", "Directory", "Directory"),
        ("Contains_File_Module", "File", "Module"),
        ("Defines_File_Function", "File", "Function"),
        ("Defines_File_Struct", "File", "Struct"),
        ("Defines_File_Enum", "File", "Enum"),
        ("Defines_File_Trait", "File", "Trait"),
        ("Defines_File_Constant", "File", "Constant"),
        ("Defines_File_TypeAlias", "File", "TypeAlias"),
        // source: B1 fix — Q9/Q14 expect File->Import edges but the rel
        // table was never registered, so Defines edges from File to Import
        // were silently dropped by is_valid_rel_table.
        ("Defines_File_Import", "File", "Import"),
        ("Defines_Module_Import", "Module", "Import"),
        ("Defines_Module_Function", "Module", "Function"),
        ("Defines_Module_Struct", "Module", "Struct"),
        ("Defines_Module_Enum", "Module", "Enum"),
        ("Defines_Module_Trait", "Module", "Trait"),
        ("Defines_Module_Constant", "Module", "Constant"),
        ("Defines_Module_TypeAlias", "Module", "TypeAlias"),
        ("HasMethod_Struct_Method", "Struct", "Method"),
        ("HasMethod_Enum_Method", "Enum", "Method"),
        ("HasMethod_Trait_Method", "Trait", "Method"),
        ("HasField_Struct_Field", "Struct", "Field"),
        ("HasField_Enum_Field", "Enum", "Field"),
        ("HasVariant_Enum_Variant", "Enum", "Variant"),
        // 3b Imports tables — source: stages/stage-3b.md §3
        ("Imports_File_File", "File", "File"),
        ("Imports_File_Module", "File", "Module"),
        ("Imports_File_Function", "File", "Function"),
        ("Imports_File_Struct", "File", "Struct"),
        ("Imports_File_Enum", "File", "Enum"),
        ("Imports_File_Trait", "File", "Trait"),
        ("Imports_File_Constant", "File", "Constant"),
        ("Imports_File_TypeAlias", "File", "TypeAlias"),
        ("Imports_Module_Function", "Module", "Function"),
        ("Imports_Module_Struct", "Module", "Struct"),
        ("Imports_Module_Enum", "Module", "Enum"),
        ("Imports_Module_Trait", "Module", "Trait"),
        ("Imports_Module_Constant", "Module", "Constant"),
        ("Imports_Module_TypeAlias", "Module", "TypeAlias"),
        // 3b Calls tables
        ("Calls_Function_Function", "Function", "Function"),
        ("Calls_Function_Method", "Function", "Method"),
        ("Calls_Method_Function", "Method", "Function"),
        ("Calls_Method_Method", "Method", "Method"),
        // 3b Implements tables
        ("Implements_Struct_Trait", "Struct", "Trait"),
        ("Implements_Enum_Trait", "Enum", "Trait"),
        // 3b Extends table
        ("Extends_Trait_Trait", "Trait", "Trait"),
        // 3b Uses tables
        ("Uses_Function_Struct", "Function", "Struct"),
        ("Uses_Function_Enum", "Function", "Enum"),
        ("Uses_Function_Trait", "Function", "Trait"),
        ("Uses_Function_TypeAlias", "Function", "TypeAlias"),
        ("Uses_Method_Struct", "Method", "Struct"),
        ("Uses_Method_Enum", "Method", "Enum"),
        ("Uses_Method_Trait", "Method", "Trait"),
        ("Uses_Method_TypeAlias", "Method", "TypeAlias"),
        ("Uses_Struct_Struct", "Struct", "Struct"),
        ("Uses_Struct_Enum", "Struct", "Enum"),
        ("Uses_Struct_Trait", "Struct", "Trait"),
        ("Uses_Field_Struct", "Field", "Struct"),
        ("Uses_Field_Enum", "Field", "Enum"),
        ("Uses_Field_Trait", "Field", "Trait"),
        ("Uses_Field_TypeAlias", "Field", "TypeAlias"),
        // 3b-v2 Layer 4/5 tables — source: stages/stage-3b-v2.md §5
        ("Calls_Function_StdlibSymbol", "Function", "StdlibSymbol"),
        ("Calls_Method_StdlibSymbol", "Method", "StdlibSymbol"),
        ("Implements_Struct_StdlibSymbol", "Struct", "StdlibSymbol"),
        ("Implements_Enum_StdlibSymbol", "Enum", "StdlibSymbol"),
        // 3c MemberOf tables
        ("MemberOf_Function_Community", "Function", "Community"),
        ("MemberOf_Method_Community", "Method", "Community"),
        ("MemberOf_Struct_Community", "Struct", "Community"),
        ("MemberOf_Enum_Community", "Enum", "Community"),
        ("MemberOf_Trait_Community", "Trait", "Community"),
        ("MemberOf_Constant_Community", "Constant", "Community"),
        ("MemberOf_TypeAlias_Community", "TypeAlias", "Community"),
        ("MemberOf_Module_Community", "Module", "Community"),
        // 3c EntryPointOf tables
        ("EntryPointOf_Function_Process", "Function", "Process"),
        ("EntryPointOf_Method_Process", "Method", "Process"),
        // 3c ParticipatesIn tables
        ("ParticipatesIn_Function_Process", "Function", "Process"),
        ("ParticipatesIn_Method_Process", "Method", "Process"),
    ]
}
