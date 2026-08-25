//! Differential tests: the gate's lexer against the real engine's.
//!
//! Split from `read_only_gate_tests` when that file crossed the §4.1 500-line
//! cap, along the seam it already had. Everything in the sibling file is a pure
//! function over query text and runs in microseconds; everything here opens a
//! LadybugDB store and asks the engine what it thinks, which is a different
//! kind of test with a different cost.

use super::*;
use crate::query_handlers::{inject_limit_if_absent, QUERY_GRAPH_ROW_LIMIT};

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

// ---------------------------------------------------------------------------
// The injected LIMIT must actually BIND — proved by execution, not by matching
// ---------------------------------------------------------------------------

/// Review finding 1. `inject_limit_if_absent` appended the clause inline, so a
/// query ending in a line comment became `... // note LIMIT 500`: the bound sat
/// INSIDE the comment, the MATCH ran unbounded, and the response still reported
/// `limit_injected: true`.
///
/// String-matching the output would not have caught it — the text contains
/// "LIMIT 500" either way. Only executing the injected query against the engine
/// and counting rows distinguishes a bound that binds from one that is
/// commented out.
#[test]
fn the_injected_limit_binds_on_a_comment_terminated_query() {
    let (_dir, store) = engine_store("inject_limit_effectiveness");
    // More rows than the bound we are about to prove.
    let rows: Vec<Vec<(String, String)>> = (0..(QUERY_GRAPH_ROW_LIMIT + 25))
        .map(|i| {
            vec![
                ("id".to_string(), format!("f{i}")),
                ("name".to_string(), format!("f{i}")),
                ("qualified_name".to_string(), format!("m.rs::f{i}")),
                ("start_line".to_string(), "1".to_string()),
                ("end_line".to_string(), "1".to_string()),
                ("visibility".to_string(), "pub".to_string()),
                ("is_async".to_string(), "false".to_string()),
            ]
        })
        .collect();
    store
        .bulk_insert_nodes(crate::graph_store::NODE_FUNCTION, &rows)
        .expect("seed");

    for query in [
        "MATCH (n:Function) RETURN n.id // note",
        // Re-review finding 1: the fixture family ended in an identifier, a
        // comment or `;` — never in a LITERAL or a BACKTICKED identifier,
        // which is exactly where trimming the masked view amputated the query.
        "MATCH (n:Function) RETURN n.id, 'tag'",
        "MATCH (n:Function) RETURN n.id AS `total`",
        "MATCH (n:Function) WHERE n.name <> 'x' RETURN n.id",
        "MATCH (n:Function) RETURN n.id, 'tag' // and a comment",
        "MATCH (n:Function) RETURN n.id // note LIMIT 3",
        "MATCH (n:Function) RETURN n.id ; // trailing terminator then a comment",
        "MATCH (n:Function) RETURN n.id /* block */",
    ] {
        let (injected, was_injected) = inject_limit_if_absent(query);
        assert!(was_injected, "no LIMIT declared in {query:?}");

        let qr = store
            .execute_read_only_query(&injected, 30_000)
            .unwrap_or_else(|e| panic!("injected query must run: {injected:?}: {e}"));
        assert_eq!(
            qr.rows.len(),
            QUERY_GRAPH_ROW_LIMIT,
            "the injected bound must actually bind for {query:?} (injected: {injected})"
        );
    }
}

/// The other half: a bound the caller declared is honoured and not replaced,
/// even when a comment trails it.
#[test]
fn a_declared_limit_is_left_alone_behind_a_comment() {
    let (_dir, store) = engine_store("declared_limit_kept");
    let rows: Vec<Vec<(String, String)>> = (0..10)
        .map(|i| {
            vec![
                ("id".to_string(), format!("g{i}")),
                ("name".to_string(), format!("g{i}")),
                ("qualified_name".to_string(), format!("m.rs::g{i}")),
                ("start_line".to_string(), "1".to_string()),
                ("end_line".to_string(), "1".to_string()),
                ("visibility".to_string(), "pub".to_string()),
                ("is_async".to_string(), "false".to_string()),
            ]
        })
        .collect();
    store
        .bulk_insert_nodes(crate::graph_store::NODE_FUNCTION, &rows)
        .expect("seed");

    let query = "MATCH (n:Function) RETURN n.id LIMIT 4 // keep mine";
    let (injected, was_injected) = inject_limit_if_absent(query);
    assert!(!was_injected, "the caller's LIMIT must be detected");
    let qr = store
        .execute_read_only_query(&injected, 30_000)
        .expect("runs");
    assert_eq!(qr.rows.len(), 4);
}

/// Evidence for widening the identifier-position rule to cover `AS <word>`.
///
/// The sigil rule (`.` / `:`) does not reach an alias, and growing the gate's
/// exemption set is security-relevant, so the question was settled by asking
/// the engine rather than by reading the grammar: `RETURN 1 AS limit` PARSES on
/// lbug 0.19.1. The shape is reachable, so `has_limit_clause` had to stop
/// reading it as a declared bound. This test fails if a future engine starts
/// rejecting it, at which point the exemption may be narrowed again.
#[test]
fn a_reserved_word_is_accepted_as_a_bare_alias() {
    let (_dir, store) = engine_store("as_limit_probe");
    store
        .execute_read_only_query("RETURN 1 AS limit", 30_000)
        .expect("`AS limit` parses — that is why the alias exemption exists");
    assert!(!crate::query_handlers::has_limit_clause(
        "RETURN 1 AS limit"
    ));
    // Backticked, it is an ordinary alias — and backticks are masked, so the
    // clause detector never sees it.
    store
        .execute_read_only_query("RETURN 1 AS `limit`", 30_000)
        .expect("a backticked reserved word is a legal alias");
    assert!(!crate::query_handlers::has_limit_clause(
        "RETURN 1 AS `limit`"
    ));
}

/// Re-review finding 4, the opposite direction of the same root cause: a REAL
/// `LIMIT` after a backticked alias was read as part of the alias, so a second
/// LIMIT was injected and the engine rejected the doubled clause — a valid,
/// bounded query made to fail.
#[test]
fn a_real_limit_after_a_backticked_alias_is_honoured() {
    let (_dir, store) = engine_store("limit_after_backticked_alias");
    let rows: Vec<Vec<(String, String)>> = (0..12)
        .map(|i| {
            vec![
                ("id".to_string(), format!("h{i}")),
                ("name".to_string(), format!("h{i}")),
                ("qualified_name".to_string(), format!("m.rs::h{i}")),
                ("start_line".to_string(), "1".to_string()),
                ("end_line".to_string(), "1".to_string()),
                ("visibility".to_string(), "pub".to_string()),
                ("is_async".to_string(), "false".to_string()),
            ]
        })
        .collect();
    store
        .bulk_insert_nodes(crate::graph_store::NODE_FUNCTION, &rows)
        .expect("seed");

    for (query, want) in [
        ("MATCH (n:Function) RETURN n.id AS `total` LIMIT 5", 5),
        ("MATCH (n:Function) RETURN n.id AS `limit` LIMIT 3", 3),
        (
            "MATCH (n:Function) WHERE n.name <> 'q' RETURN n.id LIMIT 2",
            2,
        ),
    ] {
        let (injected, was_injected) = inject_limit_if_absent(query);
        assert!(
            !was_injected,
            "the caller's LIMIT must be seen through the masked alias: {query}"
        );
        let qr = store
            .execute_read_only_query(&injected, 30_000)
            .unwrap_or_else(|e| panic!("a valid bounded query must run: {query}: {e}"));
        assert_eq!(qr.rows.len(), want, "{query}");
    }
}
