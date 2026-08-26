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
pub use context::{get_context, GetContextError, RelatedSymbol, SymbolContext};
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

    // ONE capability check, before EVERY return this function can take.
    // It has now been placed too late twice: first inside the substring arm
    // only, leaving the hybrid arm unguarded; then above the dispatch but below
    // the empty-query return, leaving that third path answering `Ok([])` to an
    // unservable filter — the same silent-empty-versus-explanatory-refusal
    // guarantee, defeated by a return the guard sat beneath. It answers a
    // question about the INDEX, not about the query, so nothing about the query
    // belongs above it.
    reject_unservable_file_filter(options, indexes.as_ref())?;

    let query_lower = query.to_lowercase();
    let terms: Vec<&str> = query_lower.split_whitespace().collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    match indexes {
        Some(ref indexes) if indexes.bm25 || indexes.vector => {
            hybrid::search_hybrid(store, query, options, indexes)
        }
        // Fallback: graph-only substring search (v1 behavior).
        _ => substring::search_substring(store, &terms, options),
    }
}

/// Refuses `label_filter: "File"` whenever no index on disk can actually serve
/// doc/prose content — whichever branch the dispatch below would take.
///
/// Three distinct situations reach it, and none of them can answer:
///   * No search index at all. The substring fallback scans
///     [`SEARCHABLE_LABELS`], which does not — and cannot — contain `File`: a
///     doc's text lives only in a BM25 `body` field, never in the graph.
///   * A search index whose BM25 half was never built.
///   * A BM25 index built before fleet-watch#112, which has no `body` field.
///     Bare directory existence cannot tell this apart from a current index,
///     which is why the question is put to the persisted schema via
///     [`bm25::indexes_doc_bodies`] rather than to the filesystem.
///
/// In every one of them an empty result would be indistinguishable from "no doc
/// matched", leaving the caller to conclude the repository holds no such doc
/// when the query was in fact never run against any doc. `search_codebase`
/// exposes `File` in its public `label_filter` enum, so a caller reaches this
/// without doing anything wrong — it earns an explanation, not silence.
///
/// The schema probe runs only when a caller actually asks for `File`, so an
/// ordinary search pays nothing for it.
fn reject_unservable_file_filter(
    options: &SearchOptions,
    indexes: Option<&HybridIndexes<'_>>,
) -> Result<(), String> {
    let is_file_filter = options
        .label_filter
        .as_deref()
        .is_some_and(|f| f.eq_ignore_ascii_case(NODE_FILE));
    if !is_file_filter {
        return Ok(());
    }
    let servable =
        indexes.is_some_and(|ix| ix.bm25 && bm25::indexes_doc_bodies(&ix.dir.join("bm25")));
    if servable {
        return Ok(());
    }
    Err(format!(
        "label_filter '{NODE_FILE}' searches doc/prose file content, which lives only in the \
         BM25 search index built by analyze_codebase — this graph has no such index, or has \
         one built before doc-content indexing existed. Re-run analyze_codebase to rebuild \
         it, or drop label_filter to search symbols."
    ))
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

    /// Review round 2, finding 4. The guard used to sit INSIDE the substring
    /// fallback arm, so it covered only the branch that cannot serve File —
    /// while the hybrid branch chose its path on `dir.join("bm25").exists()`
    /// with no capability check at all. A BM25 index built before doc-content
    /// indexing existed takes the hybrid branch, matches nothing on a field it
    /// does not have, and answers with the same silent empty result the guard
    /// was written to abolish.
    ///
    /// This test fails on the round-1 code, which returns `Ok(vec![])`.
    #[test]
    fn a_file_label_filter_against_a_pre_body_index_is_refused() {
        let (dir, store) = store_without_an_index();
        let index_dir = dir.path().join("search_index");
        bm25::build_legacy_index(&index_dir.join("bm25"));

        let options = SearchOptions {
            limit: 10,
            label_filter: Some(NODE_FILE.to_string()),
            min_score: 0.0,
        };
        let Err(err) = search_graph(&store, "playwright browser", &options, Some(&index_dir))
        else {
            panic!("an index with no doc bodies must not answer with silence");
        };
        assert!(
            err.contains("analyze_codebase"),
            "the error must say how to get a current index, got: {err}"
        );
    }

    /// The same index still serves an ordinary symbol query — the refusal is
    /// scoped to doc content, and does not make a legacy index unusable.
    #[test]
    fn a_pre_body_index_still_answers_an_unfiltered_query() {
        let (dir, store) = store_without_an_index();
        let index_dir = dir.path().join("search_index");
        bm25::build_legacy_index(&index_dir.join("bm25"));

        let options = SearchOptions {
            limit: 10,
            label_filter: None,
            min_score: 0.0,
        };
        assert!(
            search_graph(&store, "handle_tool_call", &options, Some(&index_dir)).is_ok(),
            "a legacy index must remain queryable, just without doc content"
        );
    }

    /// Review round 3, finding 4. The guard was placed above the dispatch but
    /// BELOW the empty-query early return, so a blank query slipped past it and
    /// answered `Ok([])` — the same silent-empty-versus-explanatory-refusal
    /// guarantee this PR already fixed twice, defeated a third time by a return
    /// the guard happened to sit beneath. The guard answers a question about the
    /// INDEX, so nothing about the query belongs above it.
    ///
    /// This test fails on the round-2 code, which returns `Ok(vec![])`.
    #[test]
    fn a_blank_query_does_not_smuggle_an_unservable_file_filter_past_the_guard() {
        let (_dir, store) = store_without_an_index();
        let options = SearchOptions {
            limit: 10,
            label_filter: Some(NODE_FILE.to_string()),
            min_score: 0.0,
        };
        for blank in ["", "   ", "\t\n "] {
            let Err(err) = search_graph(&store, blank, &options, None) else {
                panic!("blank query {blank:?} must not answer an unservable filter with silence");
            };
            assert!(err.contains("analyze_codebase"), "got: {err}");
        }
    }

    /// A blank query with a SERVABLE filter is still an ordinary empty result —
    /// the guard must not turn "you matched nothing" into an error.
    #[test]
    fn a_blank_query_without_an_unservable_filter_is_still_empty_not_an_error() {
        let (_dir, store) = store_without_an_index();
        let options = SearchOptions {
            limit: 10,
            label_filter: Some("Function".to_string()),
            min_score: 0.0,
        };
        let hits =
            search_graph(&store, "   ", &options, None).expect("blank query is not an error");
        assert!(hits.is_empty());
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
