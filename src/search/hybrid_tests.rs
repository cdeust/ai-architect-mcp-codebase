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
    // member_count 5 < SMALL_COMMUNITY (20), so `boost_for` pays out.
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
        .insert_edge("MemberOf_Function_Community", "main.rs::alpha", "c1", &[])
        .expect("insert MemberOf edge");
    (dir, store)
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

/// Review finding 10. `min_score` was applied to two different scales
/// depending on the kind of hit: the symbol branch compared
/// `hit.score + boost`, the File branch compared the raw `hit.score`. Since a
/// File can never receive the community/process boost (files are neither
/// clustered nor traced), a File hit at the SAME retrieval score as a symbol
/// was dropped where the symbol cleared the bar — a recall gap for any caller
/// who sets `min_score` once and expects it to mean one thing.
///
/// This test fails on the pre-fix code: with the threshold placed between the
/// raw score and the boosted score, the symbol is admitted and the File is not.
#[test]
fn min_score_admits_both_hit_kinds_on_the_same_scale() {
    let (_dir, store) = fixture();
    let boosts = RankBoosts::load(&store);

    // The Function's boost is HYBRID_WEIGHTS.small_community (0.002); the
    // threshold sits strictly between the raw score and the boosted one.
    let raw = 0.010_f64;
    let opts = options(raw + HYBRID_WEIGHTS.small_community / 2.0);

    let symbol = enrich_from_graph(&store, &hit("main.rs::alpha", raw), &boosts, &opts);
    let file = enrich_from_graph(&store, &hit("docs/guide.md", raw), &boosts, &opts);
    assert_eq!(
        symbol.is_some(),
        file.is_some(),
        "one min_score, one scale: two hits at the same retrieval score must be \
         admitted or dropped together, whatever kind they are"
    );

    // And the gate still gates: below the threshold nothing passes, above it
    // both kinds do.
    let strict = options(raw * 2.0);
    assert!(enrich_from_graph(&store, &hit("main.rs::alpha", raw), &boosts, &strict).is_none());
    assert!(enrich_from_graph(&store, &hit("docs/guide.md", raw), &boosts, &strict).is_none());

    let loose = options(0.0);
    assert!(enrich_from_graph(&store, &hit("main.rs::alpha", raw), &boosts, &loose).is_some());
    assert!(enrich_from_graph(&store, &hit("docs/guide.md", raw), &boosts, &loose).is_some());
}

/// The community boost still reaches the REPORTED score — the gate moved, the
/// ranking did not. Without this, "gate on the raw score" could be satisfied by
/// dropping the boost entirely, which would flatten the hybrid ranking.
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
