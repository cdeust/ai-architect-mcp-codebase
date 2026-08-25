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

use std::io::Read;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value as _, STORED, TEXT};
use tantivy::{doc, Index, IndexWriter, TantivyDocument};

use super::qualified_name::file_path_of;
use crate::graph_store::{GraphStore, NODE_FILE};

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

    let mut doc_count = index_symbol_docs(store, &mut writer, &fields)?;
    doc_count += index_file_docs(store, &mut writer, &fields, codebase_root)?;

    writer
        .commit()
        .map_err(|e| format!("tantivy commit: {e}"))?;
    Ok(doc_count)
}

/// Indexes one document per symbol node, keyed by its tokenized
/// `qualified_name`. Returns how many were added.
///
/// Split out of [`build_index`] when fleet-watch#112's doc/prose pass made that
/// function two phases rather than one: symbol indexing reads the graph alone,
/// doc indexing reads the working tree, and they fail for different reasons.
fn index_symbol_docs(
    store: &GraphStore,
    writer: &mut IndexWriter,
    fields: &Bm25Fields,
) -> Result<usize, String> {
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
    Ok(doc_count)
}

/// File extensions treated as prose/doc content worth indexing whole, rather
/// than parsed for symbols. Deliberately narrow and local to this module: it
/// answers "does the parser turn this into symbols" in the negative for a
/// handful of well-known doc formats, not "is this file text" in general —
/// re-indexing every source file's raw bytes here would duplicate the
/// symbol-name index at several times the size for little additional recall.
///
/// The three markdown spellings are kept in step with
/// `indexer::light_link::MD_EXTS`, which decides whether a file gets doc-link
/// edges at all: a `.markdown` file that earns a light-link edge but no BM25
/// body is findable by its references and not by its own words.
const DOC_EXTENSIONS: &[&str] = &["md", "markdown", "mdx", "txt", "rst", "adoc"];

/// Ceiling on how much of one doc file's bytes are read into the index, 256 KiB.
/// Enforced on the bytes actually read (see [`read_doc_body`]) — that is what
/// keeps one pathological file (a generated CHANGELOG, a vendored spec dump)
/// from dominating BM25 build time and index size.
///
/// source: provisional heuristic, measured 2026-08-25 against this repository's
/// own doc corpus (`git ls-files` filtered to `DOC_EXTENSIONS`): 250 files,
/// 3.86 MB total, mean 15.4 KiB, largest `.claude/agents/genius/INDEX.md` at
/// 109.4 KiB. 256 KiB therefore admits 100% of that corpus with a 2.3x margin
/// over its largest member.
///
/// Calibration plan — one corpus is a distribution, not a calibration, so this
/// value moves only on one of two measurements:
///   * RAISE it when a corpus is found whose doc files are dropped at a rate
///     that costs recall. The measurement: run `analyze_codebase` on that repo
///     and count the `File` rows this pass rejects on size alone against the
///     `bm25_doc_count` the same run reports.
///   * LOWER it when BM25 build time or on-disk index size becomes the
///     bottleneck on a doc-heavy corpus. The measurement: `SearchIndexResult`
///     already reports `elapsed_ms` and `bm25_doc_count`; pair them with the
///     size of `search_index/bm25` across a sweep of cap values over one fixed
///     repo, and take the knee.
///
/// Until one of those runs, 256 KiB rests on the distribution above and on
/// nothing else.
///
/// Three caps bound this pipeline at three different scopes, and they are
/// deliberately NOT unified: `indexer::MAX_FILE_BYTES` (10 MiB) decides which
/// files the walk visits at all, `parser::MAX_PARSE_BYTES` (1 MiB) decides
/// which are turned into symbols, and this one decides how much of a doc
/// file's text reaches BM25.
const MAX_DOC_BYTES: u64 = 262_144;

/// Indexes the full text of every `File` node whose extension is in
/// [`DOC_EXTENSIONS`] and whose on-disk size is within [`MAX_DOC_BYTES`].
/// Returns how many were added.
///
/// Two failure modes, deliberately separated (they used to share one
/// catch-all that reported a normal-looking zero):
///   * The `File` query itself failing — a schema rename, a database error —
///     is NOT a doc-coverage question and is propagated. Reporting "0 docs
///     indexed" for a broken query hides a broken index behind a plausible
///     number.
///   * One file being unreadable on disk (deleted, moved, permissions) does
///     not fail the whole build; it is skipped, and [`read_doc_body`] reports
///     it on stderr. Nothing else in this tree surfaces that condition — the
///     coverage sidecar tracks the Phase-1 parse pass, not this one — so an
///     unreported skip would be indistinguishable from a doc that simply did
///     not match the query.
///
/// Field convention, which differs from a symbol document's on purpose:
/// `qualified_name` keeps the RAW path where a symbol stores a tokenized key.
/// That field is STORED, and what is stored IS the key `hybrid::enrich_file_hit`
/// binds back to `File.id` — tokenizing it would make every doc hit
/// unresolvable. The query-side splitting a tokenized key buys is provided by
/// the two INDEXED fields instead: `name` carries the tokenized file name, and
/// `body` is prefixed with the tokenized full path, so `docs/MyDesignDoc.md`
/// still answers a "my design doc" query on both the boosted field and the body.
fn index_file_docs(
    store: &GraphStore,
    writer: &mut IndexWriter,
    fields: &Bm25Fields,
    codebase_root: &Path,
) -> Result<usize, String> {
    let cypher = format!("MATCH (n:{NODE_FILE}) RETURN n.path, n.name, n.extension");
    let qr = store
        .execute_query(&cypher)
        .map_err(|e| format!("bm25 doc pass: query {NODE_FILE} nodes: {e}"))?;

    let mut doc_count = 0usize;
    for row in &qr.rows {
        if row.len() < 3 {
            continue;
        }
        let (path, name, extension) = (&row[0], &row[1], &row[2]);
        if !DOC_EXTENSIONS
            .iter()
            .any(|ext| extension.eq_ignore_ascii_case(ext))
        {
            continue;
        }
        let Some(body) = read_doc_body(&codebase_root.join(path), path) else {
            continue;
        };
        if body.trim().is_empty() {
            continue;
        }

        writer
            .add_document(doc!(
                fields.qualified_name => path.clone(),
                fields.name => tokenize_symbol(name),
                fields.label => NODE_FILE.to_string(),
                fields.file_path => path.clone(),
                fields.body => format!("{}\n{body}", tokenize_symbol(path)),
            ))
            .map_err(|e| format!("tantivy add doc (file body): {e}"))?;
        doc_count += 1;
    }
    Ok(doc_count)
}

/// Reads `abs_path`, or `None` when it is unreadable or larger than
/// [`MAX_DOC_BYTES`]. `rel_path` names the file in the skip report.
///
/// The cap is enforced on the bytes this function actually reads, never on the
/// graph's recorded `File.size_bytes`. That column is a snapshot taken when the
/// file was indexed: a doc that grew afterwards — an actively-written CHANGELOG,
/// a regenerated spec dump — passes a stale check and is then read whole, so
/// the cap bounds nothing in exactly the case it exists for. Reading through
/// `Read::take` closes the gap with no `metadata()` pre-check, which would only
/// move the race rather than remove it. The `+ 1` is what distinguishes "exactly
/// at the cap" from "over it"; nothing past that byte enters memory.
fn read_doc_body(abs_path: &Path, rel_path: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let read = std::fs::File::open(abs_path)
        .and_then(|f| f.take(MAX_DOC_BYTES + 1).read_to_end(&mut bytes));
    if let Err(e) = read {
        eprintln!("[ap] bm25: doc file not indexed ({rel_path}): {e}");
        return None;
    }
    if bytes.len() as u64 > MAX_DOC_BYTES {
        return None;
    }
    // `from_utf8` takes ownership of the buffer on success — the common case —
    // where `from_utf8_lossy(..).into_owned()` copies it unconditionally. A
    // file that is not valid UTF-8 still falls back to lossy replacement rather
    // than being dropped from the index.
    Some(
        String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
    )
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
///
/// The query is tokenized the same way symbol names are. For a plain
/// multi-word prose query that is a no-op — `tokenize_symbol` only splits on
/// `_ : / .` and camelCase boundaries, none of which a space-separated query
/// contains — so it does not distort doc-content queries either.
///
/// Three fields are searched: `name`, `qualified_name` and (fleet-watch#112)
/// `body`. `name` is boosted 2x over the other two, so an exact symbol-name
/// match still outranks an incidental mention of the same words in a long
/// doc's prose; `body` carries no boost of its own and so ranks level with
/// `qualified_name`.
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

    let tokenized_query = tokenize_symbol(query_str);

    let mut parser = QueryParser::for_index(
        &index,
        vec![fields.name, fields.qualified_name, fields.body],
    );
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

    decode_hits(&searcher, &schema, &fields, top_docs)
}

/// Reads the stored fields of each scored hit back into a [`Bm25Result`].
///
/// Split from [`query_index`] so that function stays about building and running
/// the query (§4.2); this one is only about decoding what came back.
fn decode_hits(
    searcher: &tantivy::Searcher,
    schema: &Schema,
    fields: &Bm25Fields,
    top_docs: Vec<(f32, tantivy::DocAddress)>,
) -> Result<Vec<Bm25Result>, String> {
    let mut results = Vec::with_capacity(top_docs.len());
    for (score, doc_addr) in top_docs {
        let doc: TantivyDocument = searcher
            .doc(doc_addr)
            .map_err(|e| format!("tantivy doc retrieve: {e}"))?;
        results.push(Bm25Result {
            qualified_name: field_text(&doc, schema, fields.qualified_name),
            name: field_text(&doc, schema, fields.name),
            label: field_text(&doc, schema, fields.label),
            file_path: field_text(&doc, schema, fields.file_path),
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
