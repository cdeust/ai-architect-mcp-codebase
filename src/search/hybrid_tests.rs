//! Tests for the fused-hit enrichment path. Split from `hybrid.rs` so that
//! file stays about the ranking path itself (§4.1).
use super::*;
use crate::graph_store::{NODE_COMMUNITY, NODE_FILE, NODE_FUNCTION};

/// A graph shaped like a real index: `File.id` keeps the `src/` component the
/// parser strips when it builds `qualified_name`, and the one Function sits in
/// a community small enough to earn the hybrid path's community boost.
fn fixture() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("search_hybrid_enrich")
        .tempdir()
        .expect("tempdir");
    let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    for id in ["src/main.rs", "docs/guide.md"] {
        insert_file(&store, id);
    }
    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", "'main.rs::alpha'"),
                ("name", "'alpha'"),
                ("qualified_name", "'main.rs::alpha'"),
                ("start_line", "1"),
                ("end_line", "3"),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .expect("insert Function");
    put_in_small_community(&store, "main.rs::alpha");
    (dir, store)
}

fn insert_file(store: &GraphStore, id: &str) {
    store
        .insert_node(
            NODE_FILE,
            &[
                ("id", &format!("'{id}'")),
                ("path", &format!("'{id}'")),
                ("name", "'x'"),
                ("extension", "'md'"),
                ("size_bytes", "1"),
                ("parse_errors", "0"),
            ],
        )
        .expect("insert File");
}

/// Puts `qn` in a community of 5 — under `SMALL_COMMUNITY` (20), so
/// `boost_for` actually pays out and the threshold tests have a boost to
/// straddle.
fn put_in_small_community(store: &GraphStore, qn: &str) {
    store
        .insert_node(
            NODE_COMMUNITY,
            &[
                ("id", "'c1'"),
                ("name", "'core'"),
                ("algorithm", "'louvain'"),
                ("resolution_param", "1.0"),
                ("member_count", "5"),
                ("modularity_contribution", "0.1"),
            ],
        )
        .expect("insert Community");
    store
        .insert_edge("MemberOf_Function_Community", qn, "c1", &[])
        .expect("insert MemberOf edge");
}

fn hit(key: &str, score: f64) -> rrf::RrfResult {
    rrf::RrfResult {
        key: key.to_string(),
        score,
    }
}

fn options(min_score: f64) -> SearchOptions {
    SearchOptions {
        limit: 10,
        label_filter: None,
        min_score,
    }
}

/// Review round 2, finding 3 — the regression guard, with the review's own
/// worked example.
///
/// Round 1 moved `min_score` off the boosted score and onto the raw retrieval
/// score. `boost_for` is non-negative, so that was strictly more restrictive
/// for every symbol carrying a community or process boost: a symbol at raw
/// 0.0095 with a 0.002 small-community boost (final 0.0115) passed the
/// `min_score: 0.01` that BOTH production callers use, and stopped passing.
/// Silently narrowing pre-existing, non-File results.
///
/// This test fails on the round-1 code, which drops the symbol.
#[test]
fn a_boosted_symbol_still_clears_the_production_threshold() {
    let (_dir, store) = fixture();
    let boosts = RankBoosts::load(&store);

    // The review's numbers, verbatim: raw 0.0095, small-community boost 0.002,
    // production threshold 0.01.
    let raw = 0.0095_f64;
    const PRODUCTION_MIN_SCORE: f64 = 0.01;
    assert!(
        raw < PRODUCTION_MIN_SCORE,
        "the raw score must sit BELOW the threshold, or this proves nothing"
    );
    assert!(
        raw + HYBRID_WEIGHTS.small_community > PRODUCTION_MIN_SCORE,
        "and the boosted score must sit above it"
    );

    let enriched = enrich_from_graph(
        &store,
        &hit("main.rs::alpha", raw),
        &boosts,
        &options(PRODUCTION_MIN_SCORE),
    )
    .expect("a symbol whose boosted score clears min_score must still be returned");
    assert!(enriched.score > PRODUCTION_MIN_SCORE);
}

/// The other half of the same rule (round 1, finding 10): whatever the kind of
/// hit, the ONE threshold is compared against the ONE score the caller is
/// shown. That is the invariant a caller can actually check, and it must hold
/// for symbols and Files alike.
#[test]
fn every_returned_result_satisfies_the_threshold_it_was_gated_on() {
    let (_dir, store) = fixture();
    let boosts = RankBoosts::load(&store);
    let raw = 0.010_f64;

    for min_score in [0.0, 0.005, 0.0105, 0.02] {
        for key in ["main.rs::alpha", "docs/guide.md"] {
            if let Some(r) = enrich_from_graph(&store, &hit(key, raw), &boosts, &options(min_score))
            {
                assert!(
                    r.score >= min_score,
                    "{key} was returned at score {} under min_score {min_score}",
                    r.score
                );
            }
        }
    }

    // And the gate still gates rather than admitting everything.
    let strict = options(raw * 10.0);
    assert!(enrich_from_graph(&store, &hit("main.rs::alpha", raw), &boosts, &strict).is_none());
    assert!(enrich_from_graph(&store, &hit("docs/guide.md", raw), &boosts, &strict).is_none());
}

/// The community boost still reaches the REPORTED score. Without this, "one
/// scale" could be satisfied by dropping the boost entirely, which would
/// flatten the hybrid ranking.
#[test]
fn the_community_boost_still_tilts_the_reported_score() {
    let (_dir, store) = fixture();
    let boosts = RankBoosts::load(&store);
    let raw = 0.010_f64;
    let enriched = enrich_from_graph(&store, &hit("main.rs::alpha", raw), &boosts, &options(0.0))
        .expect("the symbol must enrich");
    assert!(
        enriched.score > raw,
        "expected the small-community boost on top of {raw}, got {}",
        enriched.score
    );
    assert_eq!(enriched.community_id.as_deref(), Some("c1"));
}

/// Review finding 6. A doc/prose BM25 hit was resolved by an EXACT `File.id`
/// match only, while `impact_target::query_file_id` — the lookup `get_impact`'s
/// File-target path uses — additionally tolerates the one leading path
/// component the parser strips. Two File resolvers accepting different key
/// shapes is how one silently stops finding files the other still finds.
///
/// This test fails on the pre-fix code: `docs/guide.md` is not a `File.id`
/// here (the id is `src/docs/guide.md`), so the exact match returned `None`
/// and the hit was dropped.
#[test]
fn a_file_hit_resolves_through_the_shared_path_stripping_fallback() {
    let (dir, store) = fixture();
    store
        .insert_node(
            NODE_FILE,
            &[
                ("id", "'src/docs/nested.md'"),
                ("path", "'src/docs/nested.md'"),
                ("name", "'nested.md'"),
                ("extension", "'md'"),
                ("size_bytes", "1"),
                ("parse_errors", "0"),
            ],
        )
        .expect("insert File");
    let boosts = RankBoosts::load(&store);

    let enriched = enrich_from_graph(&store, &hit("docs/nested.md", 0.01), &boosts, &options(0.0))
        .expect("a stripped-path File key must still resolve");
    assert_eq!(enriched.qualified_name, "src/docs/nested.md");
    assert_eq!(enriched.file_path, "src/docs/nested.md");
    assert_eq!(enriched.label, NODE_FILE);
    assert_eq!(enriched.community_id, None, "files aren't clustered");
    assert!(enriched.process_names.is_empty(), "files aren't traced");
    drop(dir);
}

/// A key that names neither a symbol nor any File still drops out.
#[test]
fn an_unbindable_key_is_dropped() {
    let (_dir, store) = fixture();
    let boosts = RankBoosts::load(&store);
    assert!(enrich_from_graph(
        &store,
        &hit("no/such/thing.md", 0.01),
        &boosts,
        &options(0.0)
    )
    .is_none());
}

/// `label_filter` still narrows to File-only doc hits.
#[test]
fn a_file_label_filter_excludes_symbol_hits() {
    let (_dir, store) = fixture();
    let boosts = RankBoosts::load(&store);
    let mut opts = options(0.0);
    opts.label_filter = Some(NODE_FILE.to_string());

    assert!(enrich_from_graph(&store, &hit("docs/guide.md", 0.01), &boosts, &opts).is_some());
    assert!(enrich_from_graph(&store, &hit("main.rs::alpha", 0.01), &boosts, &opts).is_none());
}

// ---------------------------------------------------------------------------
// Retrieval depth and early label filtering (review round 4, findings 1 and 2)
// ---------------------------------------------------------------------------

/// A fake retriever returning `total` hits of `label`, recording the depth it
/// was asked for on each call so a test can assert the escalation sequence.
fn recording_retriever<'a>(
    label: &'static str,
    total: usize,
    calls: &'a std::cell::RefCell<Vec<usize>>,
) -> impl Fn(usize) -> Result<Vec<Candidate>, String> + 'a {
    move |n: usize| {
        calls.borrow_mut().push(n);
        Ok((0..total.min(n))
            .map(|i| Candidate {
                key: format!("k{i}"),
                label: label.to_string(),
            })
            .collect())
    }
}

fn opts(limit: usize, label_filter: Option<&str>) -> SearchOptions {
    SearchOptions {
        limit,
        label_filter: label_filter.map(str::to_string),
        min_score: 0.0,
    }
}

/// Review round 4, finding 2. The previous revision widened the fetch to a flat
/// 2000 for ANY `label_filter`, but `tool_schemas.rs` documents `label_filter`
/// (Function/Method/Struct/…) as the STANDARD way to narrow a search, not a rare
/// request — so every routine call paid a 2000-candidate fetch from both
/// retrievers to find results the ordinary depth already contained.
///
/// Measured on this repository, Function is 58.3% of the 5148-document index, so
/// the first fetch is satisfied outright and must never escalate.
///
/// This test fails on the round-3 code, which asks for 2000 immediately.
#[test]
fn a_common_label_is_satisfied_by_the_first_fetch_and_never_escalates() {
    let calls = std::cell::RefCell::new(Vec::new());
    let options = opts(20, Some("Function"));
    // A pool where the requested class is plentiful, as Function is.
    let ranked = retrieve_ranked(recording_retriever("Function", 10_000, &calls), &options)
        .expect("retrieve");

    assert_eq!(
        calls.borrow().len(),
        1,
        "a plentiful class must cost exactly one fetch, got {:?}",
        calls.borrow()
    );
    assert_eq!(
        calls.borrow()[0],
        base_fetch_limit(&options),
        "and that fetch is the ordinary depth, not a widened one"
    );
    assert!(ranked.len() >= options.limit * OVERFETCH_FACTOR);
}

/// The other direction of the same trade-off: a class rare enough that the
/// ordinary depth cannot fill the pool must escalate until it can.
///
/// Measured on this repository the rarest searchable classes are Trait (0.23%)
/// and TypeAlias (0.35%) — BOTH rarer than File doc bodies (0.87%). Scoping the
/// widening to `label_filter: "File"` specifically, as the round-3 revision did
/// in spirit, would therefore have left the two rarest classes starving. The
/// escalation is keyed on how the pool actually fills, not on the label's name.
#[test]
fn a_rare_label_escalates_until_the_pool_is_deep_enough() {
    for rare in ["Trait", "TypeAlias", NODE_FILE] {
        let calls = std::cell::RefCell::new(Vec::new());
        let options = opts(20, Some(rare));
        // The retriever holds plenty of hits, but only a thin slice matches.
        let retriever = |n: usize| {
            calls.borrow_mut().push(n);
            Ok((0..n)
                .map(|i| Candidate {
                    key: format!("k{i}"),
                    // 1 in 50 carries the requested label.
                    label: if i % 50 == 0 {
                        rare.to_string()
                    } else {
                        "Function".to_string()
                    },
                })
                .collect())
        };
        let ranked = retrieve_ranked(retriever, &options).expect("retrieve");

        assert!(
            calls.borrow().len() > 1,
            "{rare}: a 1-in-50 class must escalate past the first fetch, got {:?}",
            calls.borrow()
        );
        assert!(
            ranked.len() >= options.limit * OVERFETCH_FACTOR,
            "{rare}: escalation must fill the pool, got {}",
            ranked.len()
        );
        assert!(
            *calls.borrow().last().expect("a call") <= MAX_FETCH_LIMIT,
            "{rare}: escalation must respect the ceiling"
        );
    }
}

/// Escalation stops when the retriever has nothing more to give, rather than
/// re-reading a complete result set until it hits the ceiling.
#[test]
fn escalation_stops_when_the_index_is_exhausted() {
    let calls = std::cell::RefCell::new(Vec::new());
    let options = opts(20, Some("Trait"));
    // Only 3 hits exist in the whole index, and none match.
    let ranked =
        retrieve_ranked(recording_retriever("Function", 3, &calls), &options).expect("retrieve");

    assert_eq!(
        calls.borrow().len(),
        1,
        "an exhausted retriever must not be asked again, got {:?}",
        calls.borrow()
    );
    assert!(ranked.is_empty(), "and no Trait exists to return");
}

/// Review round 4, finding 1. The default, unfiltered path is this tool's
/// primary documented use case, and its pool was sized when the index held
/// symbols and nothing else. Doc bodies are now permanently part of what it
/// draws from, competing for the same top-K on the same terms, so the same
/// 60-candidate pool yields fewer symbol candidates than it did pre-PR.
///
/// This test fails on the round-3 code, where the unfiltered depth is exactly
/// `limit * OVERFETCH_FACTOR`.
#[test]
fn the_unfiltered_pool_carries_headroom_for_the_mixed_corpus() {
    let options = opts(20, None);
    let pre_pr_depth = options.limit * OVERFETCH_FACTOR;
    assert!(
        base_fetch_limit(&options) > pre_pr_depth,
        "the default pool must be deeper than the symbols-only sizing it inherited"
    );

    // It must absorb a corpus where doc bodies outnumber symbols, i.e. keep at
    // least the pre-PR count of symbol candidates at a 50% doc share.
    let calls = std::cell::RefCell::new(Vec::new());
    let retriever = |n: usize| {
        calls.borrow_mut().push(n);
        Ok((0..n)
            .map(|i| Candidate {
                key: format!("k{i}"),
                label: if i % 2 == 0 { "Function" } else { NODE_FILE }.to_string(),
            })
            .collect())
    };
    let ranked = retrieve_ranked(retriever, &options).expect("retrieve");
    let symbol_candidates = ranked.len() / 2;
    assert!(
        symbol_candidates >= pre_pr_depth / 2,
        "at a 50% doc share the pool must still hold the pre-PR symbol count"
    );
}

/// The label filter runs BEFORE the ranking is built, so the fusion budget is
/// spent only on candidates that can survive it. Round 3 filtered during
/// enrichment, behind `rrf::fuse`'s cut to `limit * 2` — which is why widening
/// only the retriever fetch did not actually cure the starvation.
#[test]
fn the_label_filter_runs_before_the_ranking_is_built() {
    let calls = std::cell::RefCell::new(Vec::new());
    let options = opts(5, Some(NODE_FILE));
    let retriever = |n: usize| {
        calls.borrow_mut().push(n);
        Ok((0..n)
            .map(|i| Candidate {
                key: format!("k{i}"),
                label: if i % 3 == 0 { NODE_FILE } else { "Function" }.to_string(),
            })
            .collect())
    };
    let ranked = retrieve_ranked(retriever, &options).expect("retrieve");

    // Only the 1-in-3 File hits reach the ranking; the Function hits were
    // discarded before it was built, so they never consumed a rank.
    let fetched = *calls.borrow().last().expect("a call");
    let expected_files = fetched.div_ceil(3);
    assert_eq!(
        ranked.len(),
        expected_files,
        "the ranking must hold ONLY the requested class: {fetched} fetched, \
         {expected_files} of them File"
    );
    for (i, entry) in ranked.iter().enumerate() {
        assert_eq!(entry.rank, i + 1, "ranks must be dense after filtering");
    }
}

/// An unfiltered query keeps every hit, whatever its label.
#[test]
fn an_unfiltered_query_keeps_both_kinds() {
    let calls = std::cell::RefCell::new(Vec::new());
    let options = opts(4, None);
    let retriever = |n: usize| {
        calls.borrow_mut().push(n);
        Ok((0..n)
            .map(|i| Candidate {
                key: format!("k{i}"),
                label: if i % 2 == 0 { NODE_FILE } else { "Function" }.to_string(),
            })
            .collect())
    };
    let ranked = retrieve_ranked(retriever, &options).expect("retrieve");
    assert_eq!(ranked.len(), base_fetch_limit(&options));
}
