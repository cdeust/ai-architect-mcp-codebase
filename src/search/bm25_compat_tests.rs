//! Cross-version compatibility for the BM25 index (review round 2, findings
//! 1, 2 and 4).
//!
//! Split from `bm25_tests.rs` per §4.1 when these pushed it past 500 lines.
//! They share a subject the rest of that file does not: what happens when the
//! index ON DISK was written by a different build than the one reading it —
//! the case CI never exercises, because every suite builds and queries with
//! the same binary.
use super::*;
use std::fs;

// ---------------------------------------------------------------------------
// Cross-version compatibility (review round 2, findings 1 and 4)
// ---------------------------------------------------------------------------

/// Inserts one `Function` node whose `qualified_name` is `qn`, so a test can
/// ask whether a BM25 hit's key binds back to it.
fn insert_function(store: &GraphStore, qn: &str, name: &str) {
    store
        .insert_node(
            crate::graph_store::NODE_FUNCTION,
            &[
                ("id", &format!("'{qn}'")),
                ("name", &format!("'{name}'")),
                ("qualified_name", &format!("'{qn}'")),
                ("start_line", "1"),
                ("end_line", "3"),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .expect("insert Function");
}

/// Review finding 1. An index whose persisted schema has only the four
/// pre-fleet-watch#112 fields — exactly what any index built by an earlier
/// binary holds on disk — must still be queryable by this binary.
///
/// This test PANICS on the pre-fix code rather than merely failing, which is
/// the whole point: `query_index` built its `Field` handles from a fresh
/// in-memory schema and handed `body` (ordinal 4) to a `QueryParser` bound to
/// the on-disk schema, which has ordinals 0..=3. Verified against vendored
/// tantivy-0.26.1: `Schema::get_field_entry` (schema.rs:281) indexes
/// `self.0.fields[field_id]` unchecked, and
/// `QueryParser::compute_logical_ast_for_leaf` (query_parser.rs:538) calls it
/// once per default field on every query. The panic crashes the synchronous
/// stdio request with nothing surfaced to the caller, and `search_codebase`
/// never rebuilds the index, so an ordinary binary upgrade reaches it.
#[test]
fn a_pre_body_index_answers_instead_of_panicking() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_legacy_query_")
        .tempdir()
        .expect("tempdir");
    let index_dir = tmp.path().join("bm25");
    build_legacy_index(&index_dir);

    let hits = query_index(&index_dir, "handle_tool_call", 10)
        .expect("a pre-body index must answer, not fail");
    assert_eq!(
        hits.len(),
        1,
        "an index without `body` still answers over the fields it does have"
    );
    assert_eq!(hits[0].label, "Function");
    assert_eq!(hits[0].qualified_name, "src/main.rs::handle_tool_call");
}

/// Review round 2, finding 2 — proven by execution in the review, pinned here.
///
/// The stored `qualified_name` IS the key: `query_index` returns it, `hybrid`
/// fuses on it, and `enrich_symbol_hit` binds it with
/// `WHERE n.qualified_name = <key>`. Storing `tokenize_symbol(qn)` made that
/// key come back as `"main rs alpha"` — matching no row in the graph, and
/// equal to no key the vector retriever emits, so BM25-only symbol hits
/// silently failed to enrich and RRF could not fuse the two lists at all.
///
/// This test fails on the pre-fix code, which returns the tokenized form.
#[test]
fn a_symbol_hit_returns_the_key_the_graph_stores() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_symbol_key_")
        .tempdir()
        .expect("tempdir");
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");

    const QN: &str = "main.rs::handle_tool_call";
    insert_function(&store, QN, "handle_tool_call");

    let index_dir = tmp.path().join("bm25_index");
    build_index(&store, &index_dir, &root).expect("build_index");

    let hits = query_index(&index_dir, "handle_tool_call", 10).expect("query");
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].qualified_name, QN,
        "the stored key must be the graph's key verbatim, not a tokenized form"
    );

    // The key the review showed failing: bind it back exactly as
    // `enrich_symbol_hit` does, and require a row.
    let cypher = format!(
        "MATCH (n:Function) WHERE n.qualified_name = {} RETURN n.name",
        crate::graph_store::cypher_str(&hits[0].qualified_name)
    );
    let qr = store.execute_query(&cypher).expect("bind the key back");
    assert_eq!(
        qr.rows.len(),
        1,
        "the key a BM25 hit carries must resolve in the graph"
    );

    // And recall did not pay for it: the camelCase/separator splitting that
    // the tokenized key used to provide is still reachable.
    assert_eq!(
        query_index(&index_dir, "handle tool", 10)
            .expect("query")
            .len(),
        1,
        "separator-split terms must still match"
    );
}

/// Review finding 4. The capability the guard needs: an index built before
/// `body` existed cannot serve doc-content queries, and says so, rather than
/// being probed by bare directory existence.
#[test]
fn doc_body_capability_is_read_from_the_persisted_schema() {
    let tmp = tempfile::Builder::new()
        .prefix("bm25_capability_")
        .tempdir()
        .expect("tempdir");

    let legacy = tmp.path().join("legacy");
    build_legacy_index(&legacy);
    assert!(
        !indexes_doc_bodies(&legacy),
        "a four-field index carries no doc bodies"
    );

    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).expect("mk repo");
    let store = GraphStore::open_or_create(&tmp.path().join("db")).expect("open store");
    store.create_schema().expect("schema");
    let current = tmp.path().join("current");
    build_index(&store, &current, &root).expect("build_index");
    assert!(
        indexes_doc_bodies(&current),
        "an index this binary built carries doc bodies"
    );

    assert!(
        !indexes_doc_bodies(&tmp.path().join("absent")),
        "a directory holding no index carries nothing"
    );
}
