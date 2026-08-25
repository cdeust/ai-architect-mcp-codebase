// search::hybrid — BM25 + vector retrieval fused by RRF.
//
// Split from `search/mod.rs` when that file crossed the §4.1 cap. This is the
// path `search_graph` takes whenever an index was built beside the graph; the
// substring scorer next door is the fallback when one was not.

use super::enrichment::{decode_row, return_clause, BoostWeights, RankBoosts};
use super::enrichment::{lookup_community, lookup_processes};
use super::qualified_name::file_path_of;
use super::{bm25, rrf, sort_and_truncate, vector};
use super::{SearchOptions, SearchResult, SEARCHABLE_LABELS};
use crate::graph_store::{cypher_str, GraphStore};
use std::path::Path;

/// Which hybrid indexes exist beside the graph, and where they live.
///
/// Grouping them removes the `Option<&Path>` that `search_hybrid` previously
/// carried alongside two booleans that already implied it was `Some` — the two
/// `unwrap()` calls that pairing forced are gone with it.
pub(super) struct HybridIndexes<'a> {
    pub(super) dir: &'a Path,
    pub(super) bm25: bool,
    pub(super) vector: bool,
}

/// How many results each retriever is asked for, as a multiple of the caller's
/// limit. RRF needs more candidates than it returns, because a document ranked
/// poorly by one retriever and well by the other must appear in both lists to
/// be fused at all.
const OVERFETCH_FACTOR: usize = 3;

/// Boost weights for the fused path. Three orders of magnitude below the
/// substring path's: RRF scores cluster tightly around 1/(k+rank), so a nudge
/// sized for a 0..1 score would dominate the ranking rather than tilt it.
const HYBRID_WEIGHTS: BoostWeights = BoostWeights {
    small_community: 0.002,
    per_process: 0.001,
};

/// Turns a retriever's ordered qualified names into an RRF input list. Rank is
/// 1-based: RRF's denominator is `k + rank`, so a 0-based rank would give the
/// top hit a different weight than the formula intends.
fn as_ranking(qualified_names: impl IntoIterator<Item = String>) -> Vec<rrf::RankedEntry> {
    qualified_names
        .into_iter()
        .enumerate()
        .map(|(i, key)| rrf::RankedEntry { key, rank: i + 1 })
        .collect()
}

pub(super) fn search_hybrid(
    store: &GraphStore,
    query: &str,
    options: &SearchOptions,
    indexes: &HybridIndexes<'_>,
) -> Result<Vec<SearchResult>, String> {
    let fetch_limit = options.limit * OVERFETCH_FACTOR;

    let bm25_ranked = if indexes.bm25 {
        let hits = bm25::query_index(&indexes.dir.join("bm25"), query, fetch_limit)?;
        as_ranking(hits.into_iter().map(|r| r.qualified_name))
    } else {
        Vec::new()
    };

    let vector_ranked = if indexes.vector {
        let hits = vector::query_index(indexes.dir, query, fetch_limit)?;
        as_ranking(hits.into_iter().map(|r| r.qualified_name))
    } else {
        Vec::new()
    };

    // Fuse with RRF (Cormack et al. 2009, k=60).
    let mut ranking_lists: Vec<&[rrf::RankedEntry]> = Vec::new();
    if !bm25_ranked.is_empty() {
        ranking_lists.push(&bm25_ranked);
    }
    if !vector_ranked.is_empty() {
        ranking_lists.push(&vector_ranked);
    }
    if ranking_lists.is_empty() {
        return Ok(Vec::new());
    }
    let fused = rrf::fuse(&ranking_lists, options.limit * 2);

    let boosts = RankBoosts::load(store);
    let mut results: Vec<SearchResult> = fused
        .iter()
        .filter_map(|hit| enrich_from_graph(store, hit, &boosts, options))
        .collect();

    // `fused` is already deterministically ordered (rrf::fuse tie-breaks on
    // key) and enrichment preserves that order; sorting again here is what
    // makes both search paths expose the SAME documented order to the cursor.
    sort_and_truncate(&mut results, options.limit);
    Ok(results)
}

/// Attaches graph metadata and the community/process boosts to one fused hit,
/// or drops it: `None` when no searchable label holds the key, or when the
/// boosted score falls under `options.min_score`.
fn enrich_from_graph(
    store: &GraphStore,
    hit: &rrf::RrfResult,
    boosts: &RankBoosts,
    options: &SearchOptions,
) -> Option<SearchResult> {
    let escaped = cypher_str(&hit.key);

    for &label in SEARCHABLE_LABELS {
        if let Some(ref filter) = options.label_filter {
            if !filter.eq_ignore_ascii_case(label) {
                continue;
            }
        }
        let (clause, has_lines) = return_clause(label);
        let cypher =
            format!("MATCH (n:{label}) WHERE n.qualified_name = {escaped} RETURN {clause}");
        let Ok(qr) = store.execute_query(&cypher) else {
            continue;
        };
        let Some(node) = qr.rows.first().and_then(|row| decode_row(row, has_lines)) else {
            continue;
        };

        let community_id = lookup_community(store, label, &node.id);
        let process_names = lookup_processes(store, label, &node.id);
        let final_score = hit.score
            + boosts.boost_for(
                &node.qualified_name,
                community_id.as_deref(),
                &HYBRID_WEIGHTS,
            );

        if final_score < options.min_score {
            return None;
        }

        return Some(SearchResult {
            file_path: file_path_of(&node.qualified_name).to_string(),
            qualified_name: node.qualified_name,
            name: node.name,
            label: label.to_string(),
            score: final_score,
            community_id,
            process_names,
            start_line: node.start_line,
            end_line: node.end_line,
        });
    }
    None
}
