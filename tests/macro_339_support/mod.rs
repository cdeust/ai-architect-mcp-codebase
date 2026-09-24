// Shared helpers of the issue #339 macro-expansion tests.
#![allow(dead_code)]

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;

use crate::common::{self, TempDirExt};

pub struct Row {
    pub callee: String,
    pub line: u64,
    pub target: String,
    pub method: String,
    pub confidence: f64,
}

pub fn index_files(
    files: &[(&str, &str)],
) -> (GraphStore, resolver::ResolutionResult, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix("macro_expansion_339_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let src = tmp.path().join("fixture/src");
    fs::create_dir_all(&src).expect("mkdir src");
    for (name, text) in files {
        fs::write(src.join(name), text).expect("write fixture file");
    }
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&tmp.path().join("fixture"), &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let result = resolver::resolve_graph(&store).expect("resolve");
    (store, result, tmp)
}

/// Per-site rows of the macro sites, ordered by line.
pub fn macro_rows(store: &GraphStore, file: &str) -> Vec<Row> {
    let qr = store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_StdlibSymbol]->(s:StdlibSymbol) \
             WHERE cs.callee_name ENDS WITH '!' AND cs.id STARTS WITH '{file}' \
             RETURN cs.callee_name, cs.line, s.canonical_path, r.resolution_method, r.confidence \
             ORDER BY cs.line"
        ))
        .expect("query rows");
    qr.rows
        .iter()
        .map(|r| Row {
            callee: r[0].clone(),
            line: r[1].parse().expect("line"),
            target: r[2].clone(),
            method: r[3].clone(),
            confidence: r[4].parse().expect("confidence"),
        })
        .collect()
}

pub fn is_resolved(store: &GraphStore, file: &str, line: u64) -> bool {
    let qr = store
        .execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.id STARTS WITH '{file}' AND cs.line = {line} \
             RETURN cs.is_resolved"
        ))
        .expect("query is_resolved");
    qr.rows[0][0] == "true"
}

pub fn rows_on_line(rows: &[Row], line: u64) -> Vec<&Row> {
    rows.iter().filter(|r| r.line == line).collect()
}

pub fn reason_of(res: &resolver::ResolutionResult, file: &str) -> Option<String> {
    res.unresolved
        .iter()
        .find(|u| u.from_id.starts_with(file) && u.target_text == "write!")
        .map(|u| u.reason.clone())
}

/// The reason the pass gave the unresolved `write!` on `line` of `file`.
pub fn reason_on_line(res: &resolver::ResolutionResult, file: &str, line: u64) -> Option<String> {
    let marker = format!("::call@{line}:");
    res.unresolved
        .iter()
        .find(|u| {
            u.from_id.starts_with(file)
                && u.from_id.contains(&marker)
                && u.target_text.ends_with('!')
        })
        .map(|u| u.reason.clone())
}
