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

// ---------------------------------------------------------------------------
// Schema column-type map for the UNWIND bulk path.
//
// The UNWIND + Struct parameter path requires strongly-typed Value variants
// matching each column's declared type. The lookup below mirrors
// node_table_ddl() / rel_table_ddl() exactly — it is the single source of
// truth for "what LogicalType does this (label, property) expect".
// source: stages/stage-3.md §schema, stages/stage-3b.md §2, stages/stage-3c.md §4.
// ---------------------------------------------------------------------------

pub(crate) type ColTypes = &'static [(&'static str, LogicalType)];

// Schema tables, grouped by shape. Mirrors node_table_ddl() columns.
pub(crate) const COLS_DIRECTORY: ColTypes = &[
    ("id", LogicalType::String),
    ("path", LogicalType::String),
    ("name", LogicalType::String),
];
pub(crate) const COLS_FILE: ColTypes = &[
    ("id", LogicalType::String),
    ("path", LogicalType::String),
    ("name", LogicalType::String),
    ("extension", LogicalType::String),
    ("size_bytes", LogicalType::Int64),
    // source: stages/stage-3.md §10.5 — must mirror the NODE_FILE DDL.
    ("parse_errors", LogicalType::Int64),
];
// source: Spike B' BUG #5 + #9 — every symbol-bearing label gets a
// `language` String column; Struct/Enum/Trait additionally gain `bases`.
// Module intentionally has no language (it's a logical aggregation, not
// source); it still uses COLS_MODULE which keeps the pre-Spike-B' shape.
pub(crate) const COLS_MODULE: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
];
pub(crate) const COLS_VARIANT: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    // source: stages/stage-3.md §10.1 — must mirror the NODE_VARIANT DDL.
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
pub(crate) const COLS_FUNCTION: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
    ("is_async", LogicalType::Bool),
    // source: issue #92 — Uses-edge inputs (return type + constructed types).
    ("return_type", LogicalType::String),
    ("constructed_types", LogicalType::String),
    ("language", LogicalType::String),
];
pub(crate) const COLS_METHOD: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
    ("is_async", LogicalType::Bool),
    ("receiver_type", LogicalType::String),
    ("trait_name", LogicalType::String),
    // source: issue #92 — Uses-edge inputs (return type + constructed types).
    ("return_type", LogicalType::String),
    ("constructed_types", LogicalType::String),
    ("language", LogicalType::String),
];
pub(crate) const COLS_TYPEDECL: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
    ("language", LogicalType::String),
    ("bases", LogicalType::String),
    ("implements", LogicalType::String),
];
// source: stages/stage-3.md §10.1 — Field/Constant/TypeAlias/Import gain span
// columns; §10.4 — Import/CallSite gain is_resolved. Each const MUST mirror the
// corresponding node DDL exactly (column name + order feed the UNWIND type map).
pub(crate) const COLS_FIELD: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("type_annotation", LogicalType::String),
    ("visibility", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
pub(crate) const COLS_CONSTANT: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("type_annotation", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
pub(crate) const COLS_TYPE_ALIAS: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("target_type", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
pub(crate) const COLS_IMPORT: ColTypes = &[
    ("id", LogicalType::String),
    ("path", LogicalType::String),
    ("alias", LogicalType::String),
    ("is_glob", LogicalType::Bool),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("is_resolved", LogicalType::Bool),
    ("language", LogicalType::String),
];
pub(crate) const COLS_CALL_SITE: ColTypes = &[
    ("id", LogicalType::String),
    ("callee_name", LogicalType::String),
    ("line", LogicalType::Int64),
    ("col", LogicalType::Int64),
    ("is_resolved", LogicalType::Bool),
    ("language", LogicalType::String),
];
pub(crate) const COLS_COMMUNITY: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("algorithm", LogicalType::String),
    ("resolution_param", LogicalType::Double),
    ("member_count", LogicalType::Int64),
    ("modularity_contribution", LogicalType::Double),
];
pub(crate) const COLS_PROCESS: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("entry_point_id", LogicalType::String),
    ("entry_kind", LogicalType::String),
    ("entry_confidence", LogicalType::Double),
    ("depth", LogicalType::Int64),
    ("symbol_count", LogicalType::Int64),
];
// History layer — mirrors the NODE_COMMIT / NODE_VERSION DDL exactly.
pub(crate) const COLS_COMMIT: ColTypes = &[
    ("id", LogicalType::String),
    ("sha", LogicalType::String),
    ("author", LogicalType::String),
    ("author_email", LogicalType::String),
    ("committed_at", LogicalType::Int64),
    ("message", LogicalType::String),
];
pub(crate) const COLS_VERSION: ColTypes = &[
    ("id", LogicalType::String),
    ("entity_id", LogicalType::String),
    ("entity_kind", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("change_type", LogicalType::String),
    ("commit_sha", LogicalType::String),
    ("committed_at", LogicalType::Int64),
    ("lines_changed", LogicalType::Int64),
];
// Infrastructure-as-code layer (issue #63) — mirror the NODE_IAC_* DDL exactly.
pub(crate) const COLS_IAC_RESOURCE: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("resource_kind", LogicalType::String),
    ("api_version", LogicalType::String),
    ("namespace", LogicalType::String),
    ("image", LogicalType::String),
    ("ports", LogicalType::String),
    ("entrypoint", LogicalType::String),
    ("workdir", LogicalType::String),
    ("source", LogicalType::String),
    ("path", LogicalType::String),
    ("start_line", LogicalType::Int64),
];
pub(crate) const COLS_IAC_MODULE: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("resource_kind", LogicalType::String),
    ("source", LogicalType::String),
    ("path", LogicalType::String),
    ("start_line", LogicalType::Int64),
];
pub(crate) const COLS_IAC_IMAGE: ColTypes = &[
    ("id", LogicalType::String),
    ("reference", LogicalType::String),
    ("name", LogicalType::String),
    ("tag", LogicalType::String),
    ("registry", LogicalType::String),
];

pub(crate) fn node_column_types(label: &str) -> Result<ColTypes, String> {
    match label {
        NODE_DIRECTORY => Ok(COLS_DIRECTORY),
        NODE_FILE => Ok(COLS_FILE),
        NODE_MODULE => Ok(COLS_MODULE),
        NODE_VARIANT => Ok(COLS_VARIANT),
        NODE_FUNCTION => Ok(COLS_FUNCTION),
        NODE_METHOD => Ok(COLS_METHOD),
        NODE_STRUCT | NODE_ENUM | NODE_TRAIT => Ok(COLS_TYPEDECL),
        NODE_FIELD => Ok(COLS_FIELD),
        NODE_CONSTANT => Ok(COLS_CONSTANT),
        NODE_TYPE_ALIAS => Ok(COLS_TYPE_ALIAS),
        NODE_IMPORT => Ok(COLS_IMPORT),
        NODE_CALL_SITE => Ok(COLS_CALL_SITE),
        NODE_COMMUNITY => Ok(COLS_COMMUNITY),
        NODE_PROCESS => Ok(COLS_PROCESS),
        NODE_COMMIT => Ok(COLS_COMMIT),
        NODE_VERSION => Ok(COLS_VERSION),
        NODE_IAC_RESOURCE => Ok(COLS_IAC_RESOURCE),
        NODE_IAC_MODULE => Ok(COLS_IAC_MODULE),
        NODE_IAC_IMAGE => Ok(COLS_IAC_IMAGE),
        other => Err(format!("unknown node label for bulk insert: {other}")),
    }
}

/// Returns the declared property schema for an edge table. Empty for
/// untyped rel tables. source: rel_table_ddl() in this module.
pub(crate) fn edge_column_types(rel_table: &str) -> ColTypes {
    if is_resolution_rel(rel_table) || is_structural_provenance_rel(rel_table) {
        &[
            ("confidence", LogicalType::Double),
            ("resolution_method", LogicalType::String),
        ]
    } else if is_entrypoint_rel(rel_table) {
        &[("confidence", LogicalType::Double)]
    } else if is_participates_rel(rel_table) {
        &[("depth", LogicalType::Int64)]
    } else {
        &[]
    }
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
