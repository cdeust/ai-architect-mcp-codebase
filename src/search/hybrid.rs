// search::hybrid — BM25 + vector retrieval fused by RRF.
//
// Split from `search/mod.rs` when that file crossed the §4.1 cap. This is the
// path `search_graph` takes whenever an index was built beside the graph; the
// substring scorer next door is the fallback when one was not.

use super::enrichment::{decode_row, return_clause, BoostWeights, RankBoosts};
use super::enrichment::{lookup_community, lookup_processes};
use super::qualified_name::file_path_of;
use super::{bm25, impact_target, rrf, sort_and_truncate, vector};
use super::{SearchOptions, SearchResult, SEARCHABLE_LABELS};
use crate::graph_store::{cypher_str, GraphStore, NODE_FILE};
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

/// Extra depth the DEFAULT, unfiltered pool carries because the corpus this
/// tool draws from is now permanently mixed.
///
/// `OVERFETCH_FACTOR` was sized when the index held symbol documents and
/// nothing else. Since fleet-watch#112 it also holds doc/prose file bodies,
/// which compete for the same top-K on the same terms — so the same 60-candidate
/// pool now yields FEWER symbol candidates than it did before this PR, and a
/// marginal symbol that used to be fused is silently gone. That is a recall
/// regression on this tool's primary documented path ("USE THIS INSTEAD OF
/// grep/ripgrep"), not on an opt-in one.
///
/// To keep the pre-PR symbol-candidate count under a doc share of `s`, the pool
/// must grow by `1/(1-s)`. Doubling covers every corpus up to `s = 0.5` — a
/// repository whose indexed doc bodies outnumber all its symbols combined.
///
/// source: measured on this repository, 2026-08-26, from its own
/// `analyze_codebase` run — 5148 BM25 documents, of which 5103 are symbols and
/// 45 are doc bodies, i.e. `s = 0.87%`. The measured need here is a factor of
/// 1.009; 2 is deliberately far above it, because the constant has to hold for
/// documentation-heavy repositories this one is not representative of.
const MIXED_CORPUS_HEADROOM: usize = 2;

/// Ceiling on how deep [`retrieve_ranked`] will escalate for a narrow label.
///
/// Escalation exists to find a rare class, not to walk the whole index. This
/// bounds the worst case at a fixed cost; past it the honest answer is the
/// partial one, since the alternative is an unbounded scan per query.
const MAX_FETCH_LIMIT: usize = 4000;

/// The candidate pool each retriever is asked for before any escalation.
fn base_fetch_limit(options: &SearchOptions) -> usize {
    options.limit * OVERFETCH_FACTOR * MIXED_CORPUS_HEADROOM
}

/// One retriever hit, reduced to what ranking and filtering need.
pub(super) struct Candidate {
    pub(super) key: String,
    pub(super) label: String,
}

/// Whether `candidate` survives the caller's `label_filter`.
fn passes_label_filter(candidate: &Candidate, options: &SearchOptions) -> bool {
    match options.label_filter.as_deref() {
        Some(filter) => candidate.label.eq_ignore_ascii_case(filter),
        None => true,
    }
}

/// Fetches from one retriever and returns its hits as an RRF ranking, with the
/// caller's `label_filter` already applied.
///
/// Filtering HERE — at the earliest point the label is known — is the fix for
/// the starvation the previous revision only half-addressed. `label_filter`
/// used to be applied during enrichment, which sits behind TWO truncations: the
/// retriever's own top-K, and `rrf::fuse`'s cut to `limit * 2`. Widening only
/// the first left the second binding, so a deeper fetch was cut straight back
/// to 40 fused hits before the filter ever ran, and a narrow class still came
/// back empty. Neither retriever can push a label predicate down (the BM25
/// `label` field is STORED but not INDEXED, and the vector index has no
/// predicate at all), but both RETURN the label on every hit — so the filter
/// can run before the ranking is built, which is what makes the fusion budget
/// get spent on candidates that can actually survive.
///
/// Depth escalates instead of being fixed, because how deep is deep enough is a
/// property of how rare the requested class is in THIS index, which no constant
/// can know. Measured on this repository, the searchable classes span
/// Function at 58.3% of the index down to Trait at 0.23% — two and a half
/// orders of magnitude. A single widened constant is therefore wrong in both
/// directions at once: wasteful for the common labels that `tool_schemas.rs`
/// documents as the standard way to narrow a search, and still too shallow for
/// the rarest. Doubling from the ordinary depth until the pool holds enough
/// survivors self-calibrates to whatever the class share actually is, in any
/// repository, and costs a common label nothing — a `Function` filter is
/// satisfied by the first fetch and never escalates at all.
fn retrieve_ranked<F>(fetch: F, options: &SearchOptions) -> Result<Vec<rrf::RankedEntry>, String>
where
    F: Fn(usize) -> Result<Vec<Candidate>, String>,
{
    let target = options.limit * OVERFETCH_FACTOR;
    let mut fetch_limit = base_fetch_limit(options);
    loop {
        let hits = fetch(fetch_limit)?;
        let exhausted = hits.len() < fetch_limit;
        let kept: Vec<String> = hits
            .into_iter()
            .filter(|c| passes_label_filter(c, options))
            .map(|c| c.key)
            .collect();

        // Stop when the pool is deep enough, when the index has no more to
        // give, or at the ceiling. Without the `exhausted` check a narrow
        // class in a small index would escalate to MAX_FETCH_LIMIT every
        // time, re-reading the same complete result set.
        if kept.len() >= target || exhausted || fetch_limit >= MAX_FETCH_LIMIT {
            return Ok(as_ranking(kept));
        }
        fetch_limit = (fetch_limit * 2).min(MAX_FETCH_LIMIT);
    }
}

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

/// The BM25 half of the fused retrieval: label-filtered, depth-escalated, and
/// turned into an RRF ranking.
fn bm25_ranking(
    dir: &Path,
    query: &str,
    options: &SearchOptions,
) -> Result<Vec<rrf::RankedEntry>, String> {
    let bm25_dir = dir.join("bm25");
    retrieve_ranked(
        |n| {
            Ok(bm25::query_index(&bm25_dir, query, n)?
                .into_iter()
                .map(|r| Candidate {
                    key: r.qualified_name,
                    label: r.label,
                })
                .collect())
        },
        options,
    )
}

/// The vector half, on the same terms as [`bm25_ranking`].
fn vector_ranking(
    dir: &Path,
    query: &str,
    options: &SearchOptions,
) -> Result<Vec<rrf::RankedEntry>, String> {
    retrieve_ranked(
        |n| {
            Ok(vector::query_index(dir, query, n)?
                .into_iter()
                .map(|r| Candidate {
                    key: r.qualified_name,
                    label: r.label,
                })
                .collect())
        },
        options,
    )
}

pub(super) fn search_hybrid(
    store: &GraphStore,
    query: &str,
    options: &SearchOptions,
    indexes: &HybridIndexes<'_>,
) -> Result<Vec<SearchResult>, String> {
    let bm25_ranked = if indexes.bm25 {
        bm25_ranking(indexes.dir, query, options)?
    } else {
        Vec::new()
    };
    let vector_ranked = if indexes.vector {
        vector_ranking(indexes.dir, query, options)?
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

/// Attaches graph metadata to one fused hit, or drops it: `None` when neither
/// a searchable label nor a `File` holds the key, or when the resulting score
/// falls under `options.min_score`.
///
/// The two kinds of hit are enriched by the two helpers below; this function
/// owns only the gate they share and the order they are tried in.
fn enrich_from_graph(
    store: &GraphStore,
    hit: &rrf::RrfResult,
    boosts: &RankBoosts,
    options: &SearchOptions,
) -> Option<SearchResult> {
    // fleet-watch#112: a BM25 hit whose body-only match came from a doc/prose
    // File (see `bm25::index_file_docs`) has a `qualified_name` that is a file
    // path, not a symbol key — no SEARCHABLE_LABELS table can ever bind it
    // (File carries no `qualified_name` column at all; see
    // `graph_store::schema::label_has_qualified_name`). File is deliberately
    // kept OUT of SEARCHABLE_LABELS rather than added to it: that const is
    // shared with community detection and semantic diff, neither of which a
    // File belongs in. So it is a fallback here, not a ninth label.
    let enriched = enrich_symbol_hit(store, hit, boosts, options)
        .or_else(|| enrich_file_hit(store, hit, boosts, options))?;

    // ONE threshold, ONE scale, and the scale is the score the caller is
    // actually shown. Both helpers compute it the same way — the retrieval
    // score plus `boost_for` — so `min_score` means one thing across result
    // kinds, and the invariant a caller can check holds: every result returned
    // has `score >= min_score`.
    //
    // The two ways to get this wrong were both tried in review. Gating each
    // helper on ITS OWN score let a symbol clear the bar on a boosted score
    // while a File was judged on a raw one — one threshold, two scales. Hoisting
    // the gate onto the raw score ahead of enrichment made it uniform but
    // strictly narrowed pre-existing results: `boost_for` is non-negative, so
    // every symbol that used to pass on `score + boost` and now failed on
    // `score` alone was a silent recall regression (worked example: raw 0.0095
    // with a 0.002 small-community boost passed the `min_score: 0.01` both
    // production callers use, and stopped passing). Gating the final score
    // keeps symbols behaving exactly as they did before fleet-watch#112 and
    // subjects Files to the identical expression.
    if enriched.score < options.min_score {
        return None;
    }
    Some(enriched)
}

/// Binds `hit.key` to the first searchable label that holds it as a
/// `qualified_name`, and applies the community/process boosts to its score.
///
/// Membership is NOT decided here — `enrich_from_graph` owns the one
/// `min_score` gate, over the score this function computes.
fn enrich_symbol_hit(
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

/// The File-specific counterpart to [`enrich_symbol_hit`]: no
/// community/process participation (files aren't clustered or traced), no
/// line range, and `qualified_name`/`file_path` both the File's repo-relative
/// path.
///
/// The key is resolved through [`impact_target::query_file_id`] — the same
/// lookup `get_impact`'s File-target path uses — so ONE rule answers "which
/// File is this key?" across the codebase: exact `File.id` first, then the one
/// leading path component the parser strips when it builds qualified names.
/// The BM25 document is written from `File.path` while this binds `File.id`;
/// `insert_file_node` writes both from the same relative path, so they agree
/// today, and going through the shared resolver is what stops that agreement
/// from being load-bearing here.
///
/// The score is composed by the SAME expression [`enrich_symbol_hit`] uses —
/// retrieval score plus `boost_for` — so that the one `min_score` gate in
/// `enrich_from_graph` compares like with like. A File's boost is structurally
/// zero today (it belongs to no community, and only Function/Method rows enter
/// `process_counts`), but writing the expression rather than the zero is what
/// keeps the two kinds on one rule instead of two that happen to agree.
/// Membership is NOT decided here.
fn enrich_file_hit(
    store: &GraphStore,
    hit: &rrf::RrfResult,
    boosts: &RankBoosts,
    options: &SearchOptions,
) -> Option<SearchResult> {
    if let Some(ref filter) = options.label_filter {
        if !filter.eq_ignore_ascii_case(NODE_FILE) {
            return None;
        }
    }
    let id = impact_target::query_file_id(store, &hit.key)?;
    let escaped = cypher_str(&id);
    let cypher = format!("MATCH (n:{NODE_FILE}) WHERE n.id = {escaped} RETURN n.path, n.name");
    let qr = store.execute_query(&cypher).ok()?;
    let row = qr.rows.first()?;
    if row.len() < 2 {
        return None;
    }
    let path = row[0].clone();
    let score = hit.score + boosts.boost_for(&path, None, &HYBRID_WEIGHTS);
    Some(SearchResult {
        qualified_name: path.clone(),
        name: row[1].clone(),
        label: NODE_FILE.to_string(),
        file_path: path,
        score,
        community_id: None,
        process_names: Vec::new(),
        start_line: None,
        end_line: None,
    })
}

#[cfg(test)]
#[path = "hybrid_tests.rs"]
mod tests;
