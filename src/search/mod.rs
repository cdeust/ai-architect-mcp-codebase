// search — Stage 3d: hybrid BM25 + TF-IDF vector search with RRF fusion.
//
// Upgraded from substring-overlap heuristic to proper hybrid search:
//   1. BM25 full-text search via Tantivy (0.26, MIT, quickwit-oss)
//   2. Semantic vector search via TF-IDF (tokenized symbol names)
//   3. Reciprocal Rank Fusion (Cormack, Clarke, Büttcher 2009, k=60)
//
// The search index is built after clustering in the analyze_codebase flow:
//   parse → resolve → cluster → build_search_index
//
// Public API is unchanged: search_graph(store, query, options) → Vec<SearchResult>
//
// This file is the module's front door: the public types, the index-path rule,
// the index builder, and `search_graph`'s choice between the two ranking
// paths. Everything else lives in a sibling that owns one concern —
// `hybrid`/`substring` (the two ranking paths), `enrichment` (the graph-side
// data a hit carries), `context` (the 360-degree view of one symbol),
// `name_lookup` (caller input → stored key), `grouping` (the process index
// over a page), `qualified_name` (the shape of a stored key).

pub mod bm25;
pub mod context;
mod enrichment;
mod grouping;
mod hybrid;
pub mod impact_target;
mod name_lookup;
mod qualified_name;
pub mod rrf;
mod substring;
pub mod vector;
mod vector_format;

// Re-export only what a consumer actually names. `SymbolContext`,
// `CommunityRow` and `ProcessRef` are reachable as `search::context::X` (the
// module is public) rather than re-exported, so a public signature can still
// name them without this file carrying a blanket `allow(unused_imports)` to
// silence per-binary "unused re-export" warnings — that allow is what keeps
// dead public items alive.
pub use context::{get_context, GetContextError, RelatedSymbol};
pub use grouping::group_hits_by_process;
pub use impact_target::{resolve_impact_target, ImpactTarget};
pub use name_lookup::{resolve_qualified_name, SymbolNotFound};

// pub(crate): reused by prd_validator's unverifiable-file classification,
// which must apply the same src/-stripping retry this layer-2 lookup does
// before concluding a claimed file is outside the indexed graph.
pub(crate) use name_lookup::strip_leading_path_component;

use crate::graph_store::{GraphStore, NODE_FILE};
use hybrid::HybridIndexes;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Search-index path resolution — single source of truth
// ---------------------------------------------------------------------------

/// Resolves the search-index directory for a persisted graph, if one was
/// built. The index lives in a sibling `search_index/` of the graph
/// directory (built by `build_search_index` after clustering, in the
/// `analyze_codebase` flow — see this module's header comment).
///
/// Every caller of [`search_graph`] against a persisted `graph_path` MUST
/// resolve the index directory through this function rather than
/// reimplementing the sibling-path logic inline — `do_search_codebase`
/// (Stage 3d, `src/main.rs`) and `prd_input::matching::search_and_classify`
/// (Stage 4) both depend on this returning the SAME answer for the same
/// graph, otherwise one stage silently gets hybrid BM25/vector ranking
/// while the other silently falls back to substring search on identical
/// input (issue #18: Stage 4 never called this at all and always passed
/// `index_dir: None`, so it ran the substring-fallback scorer even when a
/// hybrid index existed next to the graph it was searching).
pub fn resolve_search_index_dir(graph_path: &Path) -> Option<PathBuf> {
    graph_path
        .parent()
        .map(|p| p.join("search_index"))
        .filter(|p| p.exists())
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct SearchResult {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub score: f64,
    pub community_id: Option<String>,
    pub process_names: Vec<String>,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
}

pub struct SearchOptions {
    pub limit: usize,
    pub label_filter: Option<String>,
    pub min_score: f64,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            limit: 20,
            label_filter: None,
            min_score: 0.0,
        }
    }
}

/// The node labels a query can match, in probe order.
///
/// The order is behaviour, not presentation: the lookups that stop at the
/// first hit (`name_lookup::exact_match_qn`, `context::find_community`) return
/// whichever label appears first here when a key exists under more than one.
/// It is deliberately NOT shared with `clustering::SYMBOL_LABELS`, which holds
/// the same eight labels in a different order.
pub(crate) const SEARCHABLE_LABELS: &[&str] = &[
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Trait",
    "Module",
    "Constant",
    "TypeAlias",
];

/// Imposes the ONE total order both ranking paths expose to the cursor:
/// descending score, then ascending qualified_name.
///
/// The secondary key is what makes the order total. Without it, ties fall back
/// to the unordered graph-scan input order (an engine implementation detail),
/// which would let an offset cursor over these results skip or duplicate rows.
/// `qualified_name` is unique per symbol, so the (score, qualified_name) pair
/// is a total order. source: cursor-correctness requirement
/// (response_budget::BoundedPage docs).
fn sort_and_truncate(results: &mut Vec<SearchResult>, limit: usize) {
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    results.truncate(limit);
}

// ---------------------------------------------------------------------------
// Build search index — called after clustering
// ---------------------------------------------------------------------------

/// Result of building search indexes.
pub struct SearchIndexResult {
    pub bm25_doc_count: usize,
    pub vector_doc_count: usize,
    pub elapsed_ms: u64,
}

/// Builds both BM25 (Tantivy) and vector (TF-IDF) indexes.
/// Call after the graph is fully built (post-clustering).
/// Index is stored at `<output_dir>/search_index/`.
///
/// `codebase_root` is the absolute path the graph was indexed from — BM25
/// needs it to read doc/prose files' content directly (fleet-watch#112; see
/// `bm25::index_file_docs`), since only tree-sitter-parsed files get their
/// bytes persisted in the graph itself.
///
/// SCOPE — doc/prose content reaches the LEXICAL half of hybrid retrieval only.
/// `bm25::build_index` indexes doc file bodies; `vector::build_index` below is
/// unchanged and still covers symbol nodes exclusively. So a `File` hit can
/// enter RRF fusion through the BM25 ranking and never through the vector one,
/// where a symbol hit can be reinforced by appearing in both. What a caller
/// feels: a query that overlaps a doc semantically but shares no term with it
/// lexically will not surface that doc, even though the same query would find a
/// symbol that way. This is a stated v1 boundary rather than an oversight —
/// extending TF-IDF over full doc bodies changes the vocabulary and index-size
/// characteristics of `vector`'s sparse format and belongs in its own change.
pub fn build_search_index(
    store: &GraphStore,
    output_dir: &Path,
    codebase_root: &Path,
) -> Result<SearchIndexResult, String> {
    let start = Instant::now();
    let index_dir = output_dir.join("search_index");

    let bm25_dir = index_dir.join("bm25");
    let bm25_count = bm25::build_index(store, &bm25_dir, codebase_root)?;

    let vector_count = vector::build_index(store, &index_dir)?;

    Ok(SearchIndexResult {
        bm25_doc_count: bm25_count,
        vector_doc_count: vector_count,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// search_graph — hybrid ranked search with RRF fusion
// ---------------------------------------------------------------------------

/// Searches the graph using hybrid BM25 + vector search with RRF fusion.
///
/// If search indexes exist (built by `build_search_index`), uses the hybrid
/// approach. Falls back to graph-only substring search if no indexes found.
pub fn search_graph(
    store: &GraphStore,
    query: &str,
    options: &SearchOptions,
    index_dir: Option<&Path>,
) -> Result<Vec<SearchResult>, String> {
    let query_lower = query.to_lowercase();
    let terms: Vec<&str> = query_lower.split_whitespace().collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    // The search-index directory is passed by the caller (sibling
    // ``search_index/`` of the graph dir). It used to be smuggled through the
    // process-global env var ``AA_SEARCH_INDEX_DIR``, which raced across
    // parallel callers/tests (tantivy FileDoesNotExist). Passing it as an
    // argument removes that hidden global channel entirely. source: dijkstra
    // root-cause audit of the stage3d_hybrid_search flake.
    let indexes = index_dir.map(|dir| HybridIndexes {
        dir,
        bm25: dir.join("bm25").exists(),
        vector: dir.join("vector_index.bin").exists(),
    });

    match indexes {
        Some(ref indexes) if indexes.bm25 || indexes.vector => {
            hybrid::search_hybrid(store, query, options, indexes)
        }
        // Fallback: graph-only substring search (v1 behavior).
        _ => {
            reject_unservable_file_filter(options)?;
            substring::search_substring(store, &terms, options)
        }
    }
}

/// Refuses `label_filter: "File"` on the substring fallback, which cannot
/// serve it.
///
/// That path scans [`SEARCHABLE_LABELS`], which does not — and cannot — contain
/// `File`: a doc file's text exists only in the BM25 index's `body` field, never
/// in the graph, so there is nothing on this path to match against. Returning an
/// empty result would be indistinguishable from "no doc matched", leaving the
/// caller to conclude the repository has no such doc when in fact the query was
/// never run. `search_codebase` exposes `File` in its `label_filter` enum
/// (fleet-watch#112), so a caller can reach this combination without doing
/// anything wrong — it earns an explanation, not silence.
fn reject_unservable_file_filter(options: &SearchOptions) -> Result<(), String> {
    let is_file_filter = options
        .label_filter
        .as_deref()
        .is_some_and(|f| f.eq_ignore_ascii_case(NODE_FILE));
    if is_file_filter {
        return Err(format!(
            "label_filter '{NODE_FILE}' searches doc/prose file content, which lives only in \
             the search index built by analyze_codebase — this graph has none. Rebuild it \
             with analyze_codebase, or drop label_filter to search symbols."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    fn store_without_an_index() -> (tempfile::TempDir, GraphStore) {
        let dir = tempfile::Builder::new()
            .prefix("search_no_index")
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open store");
        store.create_schema().expect("schema");
        (dir, store)
    }

    /// Review finding 2. `search_codebase` exposes `File` in its `label_filter`
    /// enum, but the substring fallback scans `SEARCHABLE_LABELS`, which cannot
    /// contain `File` — a doc's text lives only in the BM25 index. The
    /// combination therefore returned an empty result with no signal, which a
    /// caller cannot tell apart from "no doc matched".
    ///
    /// This test fails on the pre-fix code, which returns `Ok(vec![])`.
    #[test]
    fn a_file_label_filter_without_a_search_index_is_refused() {
        let (_dir, store) = store_without_an_index();
        let options = SearchOptions {
            limit: 10,
            label_filter: Some(NODE_FILE.to_string()),
            min_score: 0.0,
        };
        // `SearchResult` is not `Debug`, so unwrap the Result by hand.
        let Err(err) = search_graph(&store, "playwright browser", &options, None) else {
            panic!("an unservable filter must not answer with silence");
        };
        assert!(
            err.contains("analyze_codebase"),
            "the error must say how to get the index, got: {err}"
        );
    }

    /// The refusal is specific to the unservable combination: every other
    /// filter, and no filter at all, still falls back to substring search.
    #[test]
    fn the_substring_fallback_still_serves_symbol_filters() {
        let (_dir, store) = store_without_an_index();
        for filter in [None, Some("Function".to_string())] {
            let options = SearchOptions {
                limit: 10,
                label_filter: filter.clone(),
                min_score: 0.0,
            };
            assert!(
                search_graph(&store, "alpha", &options, None).is_ok(),
                "filter {filter:?} must still take the fallback path"
            );
        }
    }
}

#[cfg(test)]
mod label_set_tests {
    use super::SEARCHABLE_LABELS;
    use std::collections::BTreeSet;

    fn as_set(labels: &[&str]) -> BTreeSet<String> {
        labels.iter().map(|l| (*l).to_string()).collect()
    }

    /// Three modules each keep their own ORDER of the searchable symbol labels,
    /// because in each the order is behaviour — `SEARCHABLE_LABELS` and
    /// `SYMBOL_LABELS` drive first-hit-wins probes, and `DIFFABLE_LABELS` fixes
    /// a diff's section order. What must never differ is the SET: a label added
    /// to one and missed by the others silently drops that kind out of search,
    /// out of blast radius, or out of a semantic diff, with nothing failing.
    /// The orders stay independent views; this pins the set.
    #[test]
    fn the_three_label_views_hold_the_same_set() {
        let searchable = as_set(SEARCHABLE_LABELS);
        let clustering = as_set(crate::clustering::SYMBOL_LABELS);
        let diffable = as_set(crate::semantic_diff::DIFFABLE_LABELS);

        assert_eq!(
            searchable, clustering,
            "search::SEARCHABLE_LABELS and clustering::SYMBOL_LABELS drifted"
        );
        assert_eq!(
            searchable, diffable,
            "search::SEARCHABLE_LABELS and semantic_diff::DIFFABLE_LABELS drifted"
        );
    }
}
