// search::bm25 — Tantivy-backed BM25 full-text search index.
//
// Builds a Tantivy index over all symbol nodes extracted from the graph, PLUS
// (fleet-watch#112) the raw text of doc/prose files the parser does not turn
// into symbols at all (markdown, plain text, …) — those get a File-labeled
// document instead, carrying their content in `body`.
// Documents: qualified_name (stored+indexed), name (indexed, boosted),
// label (stored+faceted), file_path (stored), body (indexed, not stored —
// doc/File hits only).
//
// Source: Tantivy 0.26 (quickwit-oss, MIT). BM25 scoring is Tantivy's
// default ranking model.

use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value as _, STORED, TEXT};
use tantivy::{doc, Index, IndexWriter, TantivyDocument};

use super::qualified_name::file_path_of;
use crate::graph_store::GraphStore;

// ---------------------------------------------------------------------------
// Schema fields
// ---------------------------------------------------------------------------

pub struct Bm25Fields {
    pub qualified_name: Field,
    pub name: Field,
    pub label: Field,
    pub file_path: Field,
    pub body: Field,
}

pub fn build_schema() -> (Schema, Bm25Fields) {
    let mut builder = Schema::builder();
    let qualified_name = builder.add_text_field("qualified_name", TEXT | STORED);
    let name = builder.add_text_field("name", TEXT | STORED);
    let label = builder.add_text_field("label", STORED);
    let file_path = builder.add_text_field("file_path", STORED);
    // Indexed only, never stored: a doc's full text has no business round-
    // tripping out of a search hit (the caller already has file_path to go
    // read it), and NOT storing it keeps the index itself from duplicating
    // the size of every prose file in the repo.
    let body = builder.add_text_field("body", TEXT);
    let schema = builder.build();
    (
        schema,
        Bm25Fields {
            qualified_name,
            name,
            label,
            file_path,
            body,
        },
    )
}

// ---------------------------------------------------------------------------
// Index building
// ---------------------------------------------------------------------------

// The label set + probe ORDER both come from `search::SEARCHABLE_LABELS`. This
// file used to keep a verbatim copy, which the drift guard could not see: a
// label added to the shared const would silently stop being indexed here while
// every set-equality assertion still passed.
use super::SEARCHABLE_LABELS;

/// Builds a Tantivy BM25 index from all symbol nodes in the graph, plus
/// (fleet-watch#112) the content of doc/prose files under `codebase_root`
/// that carry no symbols at all — the parser never turns a README or a skill
/// definition into a Function/Struct/…, so without this pass their content
/// was simply absent from BM25, and a query whose only evidence lives in doc
/// prose returned nothing a code-symbol match could ever satisfy.
/// Writes the index to `index_dir`.
///
/// Idempotent: any prior contents of ``index_dir`` are removed before
/// the new index is created. Tantivy's ``Index::create_in_dir`` rejects
/// a directory that already holds an index (`Index already exists`),
/// so re-runs (e.g., ``analyze_codebase`` invoked with
/// ``force_reindex=true``) would otherwise fail. The BM25 index is a
/// derived artifact rebuilt from the live graph, so wiping is safe.
pub fn build_index(
    store: &GraphStore,
    index_dir: &Path,
    codebase_root: &Path,
) -> Result<usize, String> {
    if index_dir.exists() {
        std::fs::remove_dir_all(index_dir).map_err(|e| format!("remove stale index dir: {e}"))?;
    }
    std::fs::create_dir_all(index_dir).map_err(|e| format!("create index dir: {e}"))?;

    let (schema, fields) = build_schema();
    let index = Index::create_in_dir(index_dir, schema)
        .map_err(|e| format!("tantivy create index: {e}"))?;
    let mut writer: IndexWriter = index
        .writer(50_000_000)
        .map_err(|e| format!("tantivy writer: {e}"))?;

    let mut doc_count = 0usize;
    for &label in SEARCHABLE_LABELS {
        let cypher = format!("MATCH (n:{label}) RETURN n.qualified_name, n.name, n.id");
        let qr = match store.execute_query(&cypher) {
            Ok(qr) => qr,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 3 {
                continue;
            }
            let qn = &row[0];
            let name_val = &row[1];
            let file_path = file_path_of(qn).to_string();

            // Tokenize name by splitting on _ and :: for better BM25 matching.
            // "handle_tool_call" → "handle tool call" so BM25 finds "handle tool".
            let name_tokenized = tokenize_symbol(name_val);
            let qn_tokenized = tokenize_symbol(qn);

            writer
                .add_document(doc!(
                    fields.qualified_name => qn_tokenized,
                    fields.name => name_tokenized,
                    fields.label => label.to_string(),
                    fields.file_path => file_path,
                ))
                .map_err(|e| format!("tantivy add doc: {e}"))?;
            doc_count += 1;
        }
    }

    doc_count += index_file_docs(store, &mut writer, &fields, codebase_root)?;

    writer
        .commit()
        .map_err(|e| format!("tantivy commit: {e}"))?;
    Ok(doc_count)
}

/// File extensions treated as prose/doc content worth indexing whole, rather
/// than parsed for symbols. Deliberately narrow and local to this module: it
/// answers "does the parser turn this into symbols" in the negative for a
/// handful of well-known doc formats, not "is this file text" in general —
/// re-indexing every source file's raw bytes here would duplicate the
/// symbol-name index at several times the size for little additional recall.
const DOC_EXTENSIONS: &[&str] = &["md", "mdx", "txt", "rst", "adoc"];

/// Ceiling on how much of one doc file's bytes are read into the index.
/// 256 KiB. source: provisional heuristic — bounds one pathological file
/// (a generated CHANGELOG, a vendored spec dump) from dominating index
/// build time/size; no measured calibration yet against a real corpus.
const MAX_DOC_BYTES: u64 = 262_144;

/// Indexes the full text of every `File` node whose extension is in
/// [`DOC_EXTENSIONS`] and whose recorded size is under [`MAX_DOC_BYTES`].
/// Returns how many were added.
///
/// Best-effort like the rest of this pass: a file the graph knows about but
/// that is unreadable on disk (moved, permissions) is silently skipped
/// rather than failing the whole index build — the graph's own staleness
/// guard (fleet-watch#112's other half, `graph_freshness`) is what surfaces
/// that condition to a caller, not this pass.
fn index_file_docs(
    store: &GraphStore,
    writer: &mut IndexWriter,
    fields: &Bm25Fields,
    codebase_root: &Path,
) -> Result<usize, String> {
    let cypher = "MATCH (n:File) RETURN n.id, n.path, n.name, n.extension, n.size_bytes";
    let qr = match store.execute_query(cypher) {
        Ok(qr) => qr,
        Err(_) => return Ok(0),
    };

    let mut doc_count = 0usize;
    for row in &qr.rows {
        if row.len() < 5 {
            continue;
        }
        let (path, name, extension) = (&row[1], &row[2], &row[3]);
        let is_doc = DOC_EXTENSIONS
            .iter()
            .any(|ext| extension.eq_ignore_ascii_case(ext));
        let under_cap = row[4]
            .parse::<u64>()
            .map(|n| n <= MAX_DOC_BYTES)
            .unwrap_or(false);
        if !is_doc || !under_cap {
            continue;
        }

        let Ok(bytes) = std::fs::read(codebase_root.join(path)) else {
            continue;
        };
        let body = String::from_utf8_lossy(&bytes);
        if body.trim().is_empty() {
            continue;
        }

        writer
            .add_document(doc!(
                fields.qualified_name => path.clone(),
                fields.name => name.clone(),
                fields.label => "File".to_string(),
                fields.file_path => path.clone(),
                fields.body => body.into_owned(),
            ))
            .map_err(|e| format!("tantivy add doc (file body): {e}"))?;
        doc_count += 1;
    }
    Ok(doc_count)
}

// ---------------------------------------------------------------------------
// Querying
// ---------------------------------------------------------------------------

/// A single BM25 search result with its score.
#[allow(dead_code)]
pub struct Bm25Result {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub score: f32,
}

/// Queries the Tantivy index at `index_dir` and returns ranked results.
pub fn query_index(
    index_dir: &Path,
    query_str: &str,
    limit: usize,
) -> Result<Vec<Bm25Result>, String> {
    if !index_dir.exists() {
        return Ok(Vec::new());
    }

    let (schema, fields) = build_schema();
    let index = Index::open_in_dir(index_dir).map_err(|e| format!("tantivy open index: {e}"))?;
    let reader = index.reader().map_err(|e| format!("tantivy reader: {e}"))?;
    let searcher = reader.searcher();

    // Tokenize query the same way we tokenize symbol names. For a plain
    // multi-word prose query this is a no-op (tokenize_symbol only splits on
    // `_ : / .` and camelCase boundaries, none of which a space-separated
    // query contains), so it does not distort doc-content queries either.
    let tokenized_query = tokenize_symbol(query_str);

    let mut parser = QueryParser::for_index(
        &index,
        vec![fields.name, fields.qualified_name, fields.body],
    );
    // Boost name field 2x over qualified_name/body, so an exact symbol-name
    // match still outranks an incidental mention of the same words in a
    // long doc's prose (fleet-watch#112's body field defaults to the same
    // weight as qualified_name — no boost of its own).
    parser.set_field_boost(fields.name, 2.0);

    let query = parser
        .parse_query(&tokenized_query)
        .map_err(|e| format!("tantivy parse query: {e}"))?;

    // tantivy 0.26: `TopDocs` is a builder and no longer implements `Collector`
    // itself — an ordering must be chosen explicitly. `order_by_score()` yields
    // `Collector<Fruit = Vec<(Score, DocAddress)>>`, the same tuple shape 0.22
    // produced, so ranking is BM25-by-relevance exactly as before.
    let top_docs = searcher
        .search(&query, &TopDocs::with_limit(limit).order_by_score())
        .map_err(|e| format!("tantivy search: {e}"))?;

    let mut results = Vec::with_capacity(top_docs.len());
    for (score, doc_addr) in top_docs {
        let doc: TantivyDocument = searcher
            .doc(doc_addr)
            .map_err(|e| format!("tantivy doc retrieve: {e}"))?;
        let qn = field_text(&doc, &schema, fields.qualified_name);
        let name = field_text(&doc, &schema, fields.name);
        let label = field_text(&doc, &schema, fields.label);
        let fp = field_text(&doc, &schema, fields.file_path);

        // Reverse the tokenization for display
        results.push(Bm25Result {
            qualified_name: qn,
            name,
            label,
            file_path: fp,
            score,
        });
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn field_text(doc: &TantivyDocument, _schema: &Schema, field: Field) -> String {
    doc.get_first(field)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Tokenizes a symbol name for BM25 indexing/querying.
/// Splits on `_`, `::`, `/`, `.`, and camelCase boundaries.
/// "handle_tool_call" → "handle tool call"
/// "GraphStore" → "graph store"
/// "src/main.rs::handle_tool_call" → "src main rs handle tool call"
pub fn tokenize_symbol(s: &str) -> String {
    let mut tokens = Vec::new();
    // First split on :: / _ . /
    for part in s.split([':', '_', '/', '.']) {
        if part.is_empty() {
            continue;
        }
        // Split camelCase
        let mut current = String::new();
        for ch in part.chars() {
            if ch.is_uppercase() && !current.is_empty() {
                tokens.push(current.to_lowercase());
                current = String::new();
            }
            current.push(ch);
        }
        if !current.is_empty() {
            tokens.push(current.to_lowercase());
        }
    }
    tokens.join(" ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "bm25_tests.rs"]
mod tests;
