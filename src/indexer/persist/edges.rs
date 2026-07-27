// indexer::persist::edges — edge insertion + multi-table resolution.
//
// Extracted from indexer/persist.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move: the edge-accumulation concern (accumulate
// parsed edges, structural provenance, in-memory multi-table name resolution)
// relocated verbatim. No behavior change.

use super::SymbolBatch;
use crate::graph_store::cypher_str;
use crate::parser;
use std::collections::HashMap;

pub(super) fn accumulate_parsed_edges(
    batch: &mut SymbolBatch,
    refs: &[parser::ExtractedRef],
    label_by_qn: &HashMap<String, String>,
) {
    // source: Fermi audit — per-edge probe_node_label was firing up to 9
    // MATCH queries; the in-memory label_by_qn map eliminates them entirely.
    // Edges accumulate into the cross-file batch and flush in large bulk calls.
    for edge_ref in refs {
        let table = resolve_edge_table(
            &edge_ref.kind,
            &edge_ref.from_qualified_name,
            &edge_ref.to_qualified_name,
            label_by_qn,
        );
        let table_name = match table {
            Some(t) => t,
            None => continue,
        };
        let props = structural_provenance_props(&table_name);
        batch.push_edge(
            &table_name,
            edge_ref.from_qualified_name.clone(),
            edge_ref.to_qualified_name.clone(),
            props,
        );
    }
}

/// Structural edges (Defines_*, HasMethod_*, HasField_*, HasVariant_*) are
/// ground-truth AST facts; they carry (confidence=1.0, "direct-ast") so
/// downstream consumers see uniform provenance across structural and
/// resolution edges. Resolution edges get their provenance from the resolver.
/// source: Spike B' BUG #4 — see graph_store::is_structural_provenance_rel.
fn structural_provenance_props(table_name: &str) -> Vec<(String, String)> {
    if table_name.starts_with("Defines_")
        || table_name.starts_with("HasMethod_")
        || table_name.starts_with("HasField_")
        || table_name.starts_with("HasVariant_")
    {
        vec![
            ("confidence".to_string(), "1.0".to_string()),
            ("resolution_method".to_string(), cypher_str("direct-ast")),
        ]
    } else {
        Vec::new()
    }
}

/// Resolves the multi-table edge name using the in-memory label map.
/// This eliminates the per-edge Cypher probes that used to dominate
/// indexing cost on large codebases.
fn resolve_edge_table(
    kind: &str,
    from_qn: &str,
    to_qn: &str,
    label_by_qn: &HashMap<String, String>,
) -> Option<String> {
    match kind {
        "Defines" => resolve_defines_table(from_qn, to_qn, label_by_qn),
        "HasMethod" => resolve_has_method_table(from_qn, label_by_qn),
        "HasField" => resolve_has_field_table(from_qn, label_by_qn),
        "HasVariant" => Some("HasVariant_Enum_Variant".to_string()),
        // 3b: Extends refs are deferred to the resolver pass — skip here.
        "Extends" => None,
        _ => None,
    }
}

fn resolve_defines_table(
    from_qn: &str,
    to_qn: &str,
    label_by_qn: &HashMap<String, String>,
) -> Option<String> {
    // source: Spike B' BUG #12 fix — added Function/Method to from-candidates
    // and CallSite to to-candidates. Previously the parser-emitted
    // `Defines: Function → CallSite` edges were silently dropped here because
    // the whitelist excluded both endpoints. CallSite nodes were orphans.
    let from_label = lookup_label_among(
        from_qn,
        label_by_qn,
        &["File", "Module", "Function", "Method"],
    )?;
    let to_candidates = &[
        "Function",
        "Struct",
        "Enum",
        "Trait",
        "Constant",
        "TypeAlias",
        "Module",
        "Import",
        "CallSite",
    ];
    let to_label = lookup_label_among(to_qn, label_by_qn, to_candidates)?;
    let table = format!("Defines_{from_label}_{to_label}");
    if is_valid_rel_table(&table) {
        Some(table)
    } else {
        None
    }
}

fn resolve_has_method_table(
    from_qn: &str,
    label_by_qn: &HashMap<String, String>,
) -> Option<String> {
    let from_label = lookup_label_among(from_qn, label_by_qn, &["Struct", "Enum", "Trait"])?;
    let table = format!("HasMethod_{from_label}_Method");
    if is_valid_rel_table(&table) {
        Some(table)
    } else {
        None
    }
}

fn resolve_has_field_table(from_qn: &str, label_by_qn: &HashMap<String, String>) -> Option<String> {
    let from_label = lookup_label_among(from_qn, label_by_qn, &["Struct", "Enum"])?;
    let table = format!("HasField_{from_label}_Field");
    if is_valid_rel_table(&table) {
        Some(table)
    } else {
        None
    }
}

/// Looks up the known label for an id and returns it only if it is one of the
/// allowed candidates for the edge kind. No DB access.
fn lookup_label_among(
    id: &str,
    label_by_qn: &HashMap<String, String>,
    candidates: &[&str],
) -> Option<String> {
    let lbl = label_by_qn.get(id)?;
    if candidates.contains(&lbl.as_str()) {
        Some(lbl.clone())
    } else {
        None
    }
}

/// Checks if a rel table name exists in the known schema. Single source
/// of truth is `graph_store::REL_TABLES`; this thin shim avoids drift.
fn is_valid_rel_table(name: &str) -> bool {
    crate::graph_store::is_known_rel_table(name)
}
