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
