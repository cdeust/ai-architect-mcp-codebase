//! `check_doc_claims` handler: parses the anchored claims, checks them against
//! the graph (`doc_claims`), and shapes the answer within the response budget.

use serde_json::{json, Map, Value};
use std::path::PathBuf;

use crate::doc_claims::{self, Claim, Verdict};
use crate::graph_cache;
use crate::handler_util::require_absolute;
use crate::response_budget;
use crate::token_surface::{self, Detail};

/// Columns of a row in `format: "tabular"`, in row order.
const ROW_COLUMNS: &[&str] = &[
    "id", "file", "line", "kind", "subject", "expected", "text", "verdict", "reason", "evidence",
];

/// The fields a claim object may carry.
const CLAIM_FIELDS: [&str; 7] = ["id", "text", "file", "line", "kind", "subject", "expected"];

pub(crate) fn run_check_doc_claims(arguments: &Value) -> Value {
    match do_check_doc_claims(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "status": "error",
            "reason": "check_doc_claims_failed",
            "message": msg,
        }),
    }
}

pub(crate) fn do_check_doc_claims(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_path = required_path(args, "graph_path")?;
    let repo_root = required_path(args, "repo_root")?;
    if !repo_root.is_dir() {
        return Err(format!(
            "repo_root is not a directory: {}",
            repo_root.display()
        ));
    }
    let claims = parse_claims(args.get("claims"))?;
    let full = match args.get("detail").and_then(Value::as_str) {
        None | Some("compact") => false,
        Some("full") => true,
        Some(other) => return Err(format!("detail must be 'compact' or 'full', got '{other}'")),
    };
    let format = token_surface::parse_format(args);
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(&graph_path)?;
    let report = doc_claims::check_claims(&store, &repo_root, claims)?;

    // Rows are already in (file, line, id) order, a total order identical on
    // every call, so the cursor below neither skips nor repeats a row.
    let rows: Vec<Value> = report
        .rows
        .iter()
        .filter(|row| full || row.outcome.verdict != Verdict::Supported)
        .map(|row| row.to_json())
        .collect();
    let page =
        response_budget::bound_values_paged(rows, offset, response_budget::per_section_chars());
    let view = token_surface::render_list(&page.items, ROW_COLUMNS, "id", &Detail::Full, &format);

    let mut out = json!({
        "status": "ok",
        "tool": "check_doc_claims",
        "claim_count": report.rows.len(),
        "counts": report.counts(),
        "detail": if full { "full" } else { "compact" },
        "format": view.format,
        "row_count": page.items.len(),
        "total_rows": page.total_count,
        "offset": offset,
        "truncated": page.truncated,
        "rows": view.value,
    });
    if let Some(columns) = view.columns {
        out["columns"] = columns;
    }
    if let Some(next) = page.next_offset {
        out["next_offset"] = json!(next);
    }
    Ok(out)
}

fn required_path(args: &Map<String, Value>, field: &str) -> Result<PathBuf, String> {
    let raw = args
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required field '{field}'"))?;
    let path = require_absolute(raw, field)?;
    if !path.exists() {
        return Err(format!("{field} does not exist: {raw}"));
    }
    Ok(path)
}

fn parse_claims(raw: Option<&Value>) -> Result<Vec<Claim>, String> {
    let items = raw
        .and_then(Value::as_array)
        .ok_or("missing required field 'claims' (array of claim objects)")?;
    items
        .iter()
        .enumerate()
        .map(|(i, item)| parse_claim(i, item))
        .collect()
}

fn parse_claim(index: usize, item: &Value) -> Result<Claim, String> {
    let obj = item
        .as_object()
        .ok_or_else(|| format!("claims[{index}] must be an object"))?;
    if let Some(unknown) = obj.keys().find(|k| !CLAIM_FIELDS.contains(&k.as_str())) {
        return Err(format!("claims[{index}]: unknown field '{unknown}'"));
    }
    let text_of = |field: &str, required: bool| -> Result<String, String> {
        match obj.get(field) {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(Value::Number(n)) if field == "expected" => Ok(n.to_string()),
            None if !required => Ok(String::new()),
            None => Err(format!("claims[{index}]: missing '{field}'")),
            Some(_) => Err(format!("claims[{index}]: '{field}' must be a string")),
        }
    };
    let line = obj
        .get("line")
        .and_then(Value::as_u64)
        .filter(|l| *l >= 1)
        .ok_or_else(|| format!("claims[{index}]: 'line' must be an integer >= 1"))?;
    let id = text_of("id", false)?;
    Ok(Claim {
        id: if id.is_empty() {
            format!("c{index}")
        } else {
            id
        },
        text: text_of("text", true)?,
        file: text_of("file", true)?,
        line,
        kind: text_of("kind", true)?,
        subject: text_of("subject", false)?,
        expected: text_of("expected", false)?,
    })
}

#[cfg(test)]
#[path = "doc_claims_handlers_tests.rs"]
mod tests;
