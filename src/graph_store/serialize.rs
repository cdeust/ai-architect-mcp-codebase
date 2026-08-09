// graph_store::serialize — property ordering + typed row (un)marshalling
// for the bulk UNWIND insert path.
//
// Extracted from graph_store.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared store
// vocabulary exactly as when this lived in one module.

use super::*;

pub(crate) fn format_props(properties: &[(&str, &str)]) -> String {
    properties
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Picks the subset of columns that appear in at least one row, preserving
/// the schema's declared order. Callers may omit columns (e.g. File rows
/// without `size_bytes`) so we only bind what was actually provided.
pub(crate) fn node_prop_order(
    rows: &[Vec<(String, String)>],
    schema: ColTypes,
) -> Vec<(&'static str, LogicalType)> {
    let mut present: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for row in rows {
        for (k, _) in row {
            present.insert(k.as_str());
        }
    }
    schema
        .iter()
        .filter(|(col, _)| present.contains(*col))
        .map(|(col, ty)| (*col, ty.clone()))
        .collect()
}

/// Edge prop order — schema-driven, only bind columns present in data.
pub(crate) fn edge_prop_order(
    edges: &[PropEdge],
    schema: ColTypes,
) -> Vec<(&'static str, LogicalType)> {
    let mut present: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for e in edges {
        for (k, _) in &e.2 {
            present.insert(k.as_str());
        }
    }
    schema
        .iter()
        .filter(|(col, _)| present.contains(*col))
        .map(|(col, ty)| (*col, ty.clone()))
        .collect()
}

pub(crate) fn build_node_unwind(
    label: &str,
    prop_order: &[(&'static str, LogicalType)],
) -> (String, LogicalType) {
    let assigns: Vec<String> = prop_order
        .iter()
        .map(|(k, _)| format!("{k}: row.{k}"))
        .collect();
    let cypher = format!(
        "UNWIND $rows AS row CREATE (:{label} {{{}}})",
        assigns.join(", "),
    );
    let fields: Vec<(String, LogicalType)> = prop_order
        .iter()
        .map(|(k, ty)| ((*k).to_string(), ty.clone()))
        .collect();
    (cypher, LogicalType::Struct { fields })
}

pub(crate) fn build_edge_unwind(
    rel_table: &str,
    from_label: &str,
    to_label: &str,
    prop_order: &[(&'static str, LogicalType)],
) -> (String, LogicalType) {
    let props_clause = if prop_order.is_empty() {
        String::new()
    } else {
        let assigns: Vec<String> = prop_order
            .iter()
            .map(|(k, _)| format!("{k}: row.{k}"))
            .collect();
        format!(" {{{}}}", assigns.join(", "))
    };
    // source: Kuzu primary-key index scan. An inline PK predicate
    // `(n:Label {id: expr})` seeks the PK index (one node); the comma form
    // `MATCH (a:A), (b:B) WHERE a.id=.. AND b.id=..` plans as a CrossProduct
    // over ALL A×B nodes filtered by string Equals. On a full-dependency
    // graph (100k+ nodes) that cross product ran 5h+ at 100% CPU / 10.9G
    // before this fix (measured 2026-07-04, `sample` of hung pid 41120:
    // CrossProduct -> Filter selectUnFlatFlat<string_t,Equals> -> memcmp).
    let cypher = format!(
        "UNWIND $rows AS row \
         MATCH (a:{from_label} {{id: row.from}}) \
         MATCH (b:{to_label} {{id: row.to}}) \
         CREATE (a)-[:{rel_table}{props_clause}]->(b)",
    );
    let mut fields: Vec<(String, LogicalType)> = vec![
        ("from".to_string(), LogicalType::String),
        ("to".to_string(), LogicalType::String),
    ];
    for (k, ty) in prop_order {
        fields.push(((*k).to_string(), ty.clone()));
    }
    (cypher, LogicalType::Struct { fields })
}

pub(crate) fn build_struct_rows(
    chunk: &[Vec<(String, String)>],
    prop_order: &[(&'static str, LogicalType)],
) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(chunk.len());
    for row in chunk {
        let mut fields: Vec<(String, Value)> = Vec::with_capacity(prop_order.len());
        for (col, ty) in prop_order {
            let lit = row.iter().find(|(k, _)| k == *col).map(|(_, v)| v.as_str());
            fields.push(((*col).to_string(), literal_to_value(lit, ty, col)?));
        }
        out.push(Value::Struct(fields));
    }
    Ok(out)
}

pub(crate) fn build_edge_struct_rows(
    edges: &[PropEdge],
    prop_order: &[(&'static str, LogicalType)],
) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(edges.len());
    for (from, to, props) in edges {
        let mut fields: Vec<(String, Value)> = vec![
            ("from".to_string(), Value::String(from.clone())),
            ("to".to_string(), Value::String(to.clone())),
        ];
        for (col, ty) in prop_order {
            let lit = props
                .iter()
                .find(|(k, _)| k == *col)
                .map(|(_, v)| v.as_str());
            fields.push(((*col).to_string(), literal_to_value(lit, ty, col)?));
        }
        out.push(Value::Struct(fields));
    }
    Ok(out)
}

/// Converts a caller-supplied Cypher literal (the legacy pre-UNWIND format:
/// `'foo'` for strings, `10` for ints, `true`/`false` for bools, `1.23` for
/// doubles) into a typed `Value` matching the declared column type.
///
/// A missing literal yields a typed `Value::Null` — lbug accepts NULL in
/// typed columns. Parsing preserves the security guarantees of cypher_str
/// because the string payload is now passed as a typed parameter, not
/// interpolated into Cypher text.
pub(crate) fn literal_to_value(
    lit: Option<&str>,
    ty: &LogicalType,
    col: &str,
) -> Result<Value, String> {
    let Some(raw) = lit else {
        return Ok(Value::Null(ty.clone()));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null(ty.clone()));
    }
    match ty {
        LogicalType::String => Ok(Value::String(unwrap_cypher_string(trimmed))),
        LogicalType::Int64 => trimmed
            .parse::<i64>()
            .map(Value::Int64)
            .map_err(|e| format!("column {col}: expected INT64, got {raw:?}: {e}")),
        LogicalType::Bool => match trimmed.to_ascii_lowercase().as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" | "" => Ok(Value::Bool(false)),
            _ => Err(format!("column {col}: expected BOOL, got {raw:?}")),
        },
        LogicalType::Double => trimmed
            .parse::<f64>()
            .map(Value::Double)
            .map_err(|e| format!("column {col}: expected DOUBLE, got {raw:?}: {e}")),
        other => Err(format!(
            "column {col}: unsupported bulk-insert type {other:?}"
        )),
    }
}

/// Inverse of `cypher_str`: takes a caller-supplied literal (either an
/// already-quoted Cypher string like `'foo\'s'` or a bare value) and
/// returns the raw string content. The unescape rules mirror cypher_str:
///   \\ → \    \'  → '
/// Any unquoted input is returned as-is (callers sometimes pass raw
/// strings for non-id columns; treat those as literal contents).
pub(crate) fn unwrap_cypher_string(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                match chars.next() {
                    Some('\\') => out.push('\\'),
                    Some('\'') => out.push('\''),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        s.to_string()
    }
}

pub(crate) fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Int64(n) => n.to_string(),
        Value::Int32(n) => n.to_string(),
        Value::Int16(n) => n.to_string(),
        Value::Int8(n) => n.to_string(),
        Value::UInt64(n) => n.to_string(),
        Value::UInt32(n) => n.to_string(),
        Value::UInt16(n) => n.to_string(),
        Value::UInt8(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        _ => format!("{v:?}"),
    }
}

pub(crate) fn value_to_u64(v: &Value) -> u64 {
    match v {
        Value::Int64(n) => *n as u64,
        Value::UInt64(n) => *n,
        _ => 0,
    }
}
