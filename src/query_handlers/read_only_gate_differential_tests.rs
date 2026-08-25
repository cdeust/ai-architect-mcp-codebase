//! Differential tests: the gate's lexer against the real engine's.
//!
//! Split from `read_only_gate_tests` when that file crossed the §4.1 500-line
//! cap, along the seam it already had. Everything in the sibling file is a pure
//! function over query text and runs in microseconds; everything here opens a
//! LadybugDB store and asks the engine what it thinks, which is a different
//! kind of test with a different cost.

use super::*;

// ---------------------------------------------------------------------------
// B.2 — differential: the gate's lexer must agree with the engine's
//
// `mask_non_executable` decides what text can execute, and every refusal this
// gate makes rests on that answer being the SAME answer lbug's parser gives.
// Nothing enforced that. The two are exercised here on one set of fixtures, so
// a future lbug bump whose lexer treats an escape or a comment differently
// breaks a test instead of silently opening a hole: if the engine were to stop
// seeing a region as a literal while the gate still masks it, a mutation
// keyword hidden there would reach the parser unrefused.
// ---------------------------------------------------------------------------

/// Payloads whose bytes exercise the escape grammar both sides must share.
/// Each is a RAW value; `cypher_str` is what turns it into a literal.
const ESCAPE_FIXTURES: &[&str] = &[
    r"plain",
    r"has 'quote'",
    r"has \backslash",
    r"trailing backslash \",
    r"escaped quote \' then DETACH DELETE n",
    r"double \\ backslash",
    r#"double "quote" inside"#,
    r"comment-ish /* not a comment */ text",
    r"line-comment-ish // not a comment",
    r"semicolon ; DROP TABLE x",
];

fn engine_store(prefix: &str) -> (tempfile::TempDir, crate::graph_store::GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir");
    let store = crate::graph_store::GraphStore::open_or_create(&dir.path().join("db"))
        .expect("open_or_create");
    store.create_schema().expect("create_schema");
    (dir, store)
}

/// The engine must read back exactly the bytes `cypher_str` encoded, and the
/// gate must treat that same literal as non-executable. Disagreement in either
/// direction is a hole: the gate masking what the engine executes hides a
/// mutation, and the engine mangling what the gate believes it quoted means
/// `cypher_str` is no longer the right encoder.
#[test]
fn gate_and_engine_agree_on_the_escape_grammar() {
    let (_dir, store) = engine_store("gate_engine_escapes");
    for raw in ESCAPE_FIXTURES {
        let literal = crate::graph_store::cypher_str(raw);
        let query = format!("RETURN {literal} AS s");

        // Engine side: the literal round-trips to the original bytes.
        let qr = store
            .execute_read_only_query(&query, 30_000)
            .unwrap_or_else(|e| panic!("engine refused a pure RETURN of {raw:?}: {e}"));
        let got = qr
            .rows
            .first()
            .and_then(|r| r.first())
            .unwrap_or_else(|| panic!("no row for {raw:?}"));
        assert_eq!(
            got, raw,
            "engine decoded {literal} differently than cypher_str encoded it"
        );

        // Gate side: nothing inside that literal is executable, so a mutation
        // keyword hidden in the payload cannot trip the keyword scan.
        assert_eq!(
            forbidden_cypher_keyword(&query),
            None,
            "the gate must see the literal {literal} as data"
        );
        let masked = mask_non_executable(&query).expect("terminated literal");
        assert!(
            masked.contains("RETURN") && !masked.contains("DETACH"),
            "the literal's bytes must be masked out: {masked}"
        );
    }
}

/// Comments are the other region both sides must classify identically. The
/// engine ignoring a comment the gate did NOT mask would be harmless noise;
/// the engine EXECUTING text the gate masked as a comment is the hole.
#[test]
fn gate_and_engine_agree_on_comment_grammar() {
    let (_dir, store) = engine_store("gate_engine_comments");
    for query in [
        "RETURN 1 AS x // CREATE (n:Function)",
        "/* CREATE (n:Function) */ RETURN 1 AS x",
        "RETURN 1 AS x /* DETACH DELETE n */",
    ] {
        let qr = store
            .execute_read_only_query(query, 30_000)
            .unwrap_or_else(|e| panic!("engine refused {query:?}: {e}"));
        assert_eq!(
            qr.rows.first().and_then(|r| r.first()).map(String::as_str),
            Some("1"),
            "the engine must ignore the commented-out mutation in {query:?}"
        );
        assert_eq!(
            forbidden_cypher_keyword(query),
            None,
            "the gate must ignore it too: {query:?}"
        );
    }

    // A lone `/` is division, not a comment — both sides must agree there too,
    // and the gate must still see the mutation that follows it.
    assert!(forbidden_cypher_keyword("RETURN 1 / 1 AS x CREATE (n:Function)").is_some());
}
