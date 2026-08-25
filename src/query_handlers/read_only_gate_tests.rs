//! Tests for the `query_graph` read-only gate, in a sibling file per the
//! module convention already used by `query_handlers_tests.rs`.

use super::*;
use crate::query_handlers::{do_query_graph, run_query_graph};
use serde_json::json;

#[test]
fn chained_statements_get_a_named_reason_instead_of_an_engine_message() {
    // fleet-watch review finding 9 (regression). `execute_read_only_query`
    // must `prepare` to obtain the engine's read-only classification, and
    // `prepare` refuses a chain — so a `;`-chained request that the pre-gate
    // `Connection::query` path accepted now fails. It failed by leaking the
    // engine's "We do not support prepare multiple statements", with the
    // generic `query_failed` reason. Name the contract instead.
    assert!(!is_multi_statement("MATCH (n:Function) RETURN n.id"));
    // A trailing terminator is a single statement — `prepare` accepts it
    // (measured 2026-08-24, lbug 0.19.1), so it must not be refused here.
    assert!(!is_multi_statement("MATCH (n:Function) RETURN n.id;"));
    assert!(!is_multi_statement("MATCH (n:Function) RETURN n.id ;  "));
    // A `;` inside a literal is data, not a separator.
    assert!(!is_multi_statement(
        "MATCH (n) WHERE n.name = 'a;b' RETURN n"
    ));
    assert!(!is_multi_statement(
        "MATCH (n) RETURN n // trailing ; comment"
    ));
    // Real chains are chains.
    assert!(is_multi_statement(
        "MATCH (n) RETURN n.id; MATCH (m) RETURN m.id"
    ));
    // Fail closed: an unterminated literal hides whatever follows it.
    assert!(is_multi_statement("MATCH (n) WHERE n.a = 'unterminated"));

    // End to end: the reason code reaches the caller, and the gate runs
    // before any filesystem check so a nonexistent path still reports it.
    let out = run_query_graph(&json!({
        "graph_path": "/nonexistent/graph",
        "query": "MATCH (n) RETURN n.id; MATCH (m) RETURN m.id",
    }));
    assert_eq!(out["status"], "error");
    assert_eq!(out["reason"], "multi_statement_not_supported");
}

#[test]
fn readonly_gate_refuses_queries_over_the_import_node_table() {
    // fleet-watch review finding 1 (regression). `IMPORT` is both a
    // data-movement statement AND this schema's `Import` node table, and the
    // scan looked only at the byte immediately left of a match (and only for
    // `.`). Every query over `Import` was therefore refused — including the
    // exact shape the accuracy corpora are written against, so the whole
    // unresolved-import surface was unreachable through `query_graph`.
    for allowed in [
        "MATCH (i:Import) RETURN i.path",
        // The shape used verbatim by benches/corpora/*/ground_truth.json.
        "MATCH (f:File)-[:Defines_File_Import]->(n:Import) \
         WHERE f.path = 'app.ts' AND n.is_resolved = false RETURN n.path",
        // Whitespace between the sigil and the label is legal Cypher.
        "MATCH (i : Import) RETURN i.path",
        // A property spelled with spaces around the dot is still a property.
        "MATCH (n) RETURN n . import",
    ] {
        assert_eq!(
            forbidden_cypher_keyword(allowed),
            None,
            "must be allowed: {allowed}"
        );
    }

    // The exemption is for identifier positions only: the statements the gate
    // exists to refuse are still refused, in the same query shapes.
    for refused in [
        ("IMPORT DATABASE '/tmp/payload'", "IMPORT"),
        (
            "MATCH (i:Import) RETURN i.path ; IMPORT DATABASE '/tmp/p'",
            "IMPORT",
        ),
        ("MATCH (i:Import) DETACH DELETE i", "DELETE"),
        (
            "MATCH (i:Import) WITH i COPY (MATCH (n) RETURN n.id) TO '/tmp/x'",
            "COPY",
        ),
    ] {
        assert_eq!(
            forbidden_cypher_keyword(refused.0),
            Some(refused.1),
            "must stay refused: {}",
            refused.0
        );
    }

    // Whole-word matching must not have regressed: a keyword directly after a
    // non-sigil, non-identifier byte is still a clause.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) WITH 1 AS x CREATE (m:File)"),
        Some("CREATE")
    );
}

#[test]
fn readonly_gate_ignores_keywords_inside_literals_and_properties() {
    // issue #200 — a client that looks a symbol up BY NAME sends the symbol as
    // a string literal. `load`, `set`, `create`, `delete` and `call` are all
    // ordinary function names, so the literal must not trip the gate. Observed
    // 6x on automatised-pipeline / 4x on cortex-viz in the 2026-08-06 bench.
    for name in [
        "load", "set", "create", "delete", "merge", "drop", "call", "remove", "alter",
    ] {
        let q = format!("MATCH (s:Symbol) WHERE s.name = '{name}' RETURN s");
        assert_eq!(
            forbidden_cypher_keyword(&q),
            None,
            "literal '{name}' must pass"
        );
    }
    // Double-quoted literals and IN-lists take the same path.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (s) WHERE s.name = \"load\" RETURN s"),
        None
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (s) WHERE s.name IN ['load','parse'] RETURN s"),
        None
    );
    // Property access: `n.set` is a property, never a SET clause.
    assert_eq!(forbidden_cypher_keyword("MATCH (n) RETURN n.load"), None);
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) RETURN n.set, n.create"),
        None
    );
    // Backtick-quoted identifiers are data, not clauses.
    assert_eq!(forbidden_cypher_keyword("MATCH (n) RETURN n.`set`"), None);
    // Comments are not executable.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) RETURN n // DELETE n"),
        None
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) /* CREATE (x) */ RETURN n"),
        None
    );
}

#[test]
fn readonly_gate_blocks_filesystem_writing_statements() {
    // fleet-watch#15 — lbug's `PreparedStatement::is_read_only()` classifies
    // `COPY (..) TO 'file'` as read-only (it reads the database and writes the
    // FILESYSTEM; measured 2026-08-24 on lbug 0.19.1), so the engine gate in
    // `execute_read_only_query` cannot block the attacker-named file write.
    // The lexical gate is the only reliable barrier for data-movement
    // statements; this pins each keyword.
    assert_eq!(
        forbidden_cypher_keyword("COPY (MATCH (n) RETURN n.id) TO '/tmp/x.csv'"),
        Some("COPY")
    );
    assert_eq!(
        forbidden_cypher_keyword("EXPORT DATABASE '/tmp/exfil'"),
        Some("EXPORT")
    );
    assert_eq!(
        forbidden_cypher_keyword("IMPORT DATABASE '/tmp/payload'"),
        Some("IMPORT")
    );
    assert_eq!(
        forbidden_cypher_keyword("ATTACH '/tmp/other.db' AS other"),
        Some("ATTACH")
    );
    // A COPY chained behind a benign read by `;` is caught the same way — the
    // scan is position-independent (cortex-16 vigilance #1).
    assert_eq!(
        forbidden_cypher_keyword(
            "MATCH (n) RETURN n.id ; COPY (MATCH (m) RETURN m.id) TO '/tmp/x.csv'"
        ),
        Some("COPY")
    );
    // Masking still protects legitimate reads: `copy`/`export` as a literal or
    // a property name must not trip the gate (same contract as issue #200).
    assert_eq!(
        forbidden_cypher_keyword("MATCH (s) WHERE s.name = 'copy' RETURN s"),
        None
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) RETURN n.copy, n.export"),
        None
    );

    // End-to-end: do_query_graph refuses before `GraphStore::open_or_create`,
    // so no real DB is needed and nothing can reach the filesystem.
    for q in [
        "COPY (MATCH (n) RETURN n.id) TO '/tmp/escape.csv'",
        "EXPORT DATABASE '/tmp/exfil'",
        "IMPORT DATABASE '/tmp/payload'",
        "ATTACH '/tmp/other.db' AS other",
    ] {
        let args = json!({
            "graph_path": "/nonexistent/graph",
            "query": q
        });
        let err = do_query_graph(&args).expect_err("must reject data-movement query");
        assert!(
            err.contains("read_only_query_required"),
            "query `{q}` got: {err}"
        );
    }
}

#[test]
fn readonly_gate_still_rejects_real_mutations_around_literals() {
    // The masking must not become a bypass: a real clause OUTSIDE a literal is
    // still caught, including when a literal or comment precedes it.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (s) WHERE s.name = 'load' DELETE s"),
        Some("DELETE")
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) /* harmless */ CREATE (m:Foo)"),
        Some("CREATE")
    );
    assert_eq!(
        forbidden_cypher_keyword("// comment\nMATCH (n) SET n.x = 1"),
        Some("SET")
    );
    // An escaped quote must not end the literal early and expose the rest.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (s) WHERE s.name = 'it\\'s load' RETURN s"),
        None
    );
    // Genuine LOAD is still a mutation-class clause.
    assert_eq!(
        forbidden_cypher_keyword("LOAD CSV FROM 'x' AS r RETURN r"),
        Some("LOAD")
    );
}

#[test]
fn readonly_gate_treats_a_lone_slash_as_arithmetic_not_a_comment() {
    // Only `//` and `/*` open a comment. A single `/` is division (or part of
    // a path) and must stay executable text: mutation testing showed that
    // relaxing the block-comment guard made every lone `/` open an
    // unterminated comment, which fails the query closed for no reason.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) RETURN n.size / 2"),
        None
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) WHERE n.a / n.b > 1 RETURN n"),
        None
    );
    // A division does not hide a following mutation from the scan either.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) WITH n.a / 2 AS x DELETE n"),
        Some("DELETE")
    );
}

#[test]
fn readonly_gate_block_comment_boundaries() {
    // Discriminating inputs found by an exhaustive differential run over the
    // block-comment scanner (every string of length 0..=7 in the alphabet that
    // drives it). Without these, three arithmetic mutants on the `/*` arm and
    // its `i += 2` survive — and one of them MIS-MASKS text, which is the
    // direction that could hide a mutation from the scan.
    // # source: measured 2026-08-07, /tmp/exh200b.rs differential harness.

    // Empty block comment is a real, closed comment: masked, query allowed.
    assert_eq!(forbidden_cypher_keyword("MATCH (n) /**/ RETURN n"), None);
    // ...and it must not swallow what follows it.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) /**/ DELETE n"),
        Some("DELETE")
    );
    // `/*` with nothing after it is unterminated -> fail closed.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) RETURN n /*"),
        Some("UNTERMINATED_LITERAL")
    );
    // `/*/` LOOKS closed but is not: the `/` is part of the opener, so the
    // comment never terminates. Masking it as if closed would hide the rest.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) /*/ RETURN n"),
        Some("UNTERMINATED_LITERAL")
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) /*/ DELETE n"),
        Some("UNTERMINATED_LITERAL")
    );
    // A closed comment immediately followed by another one still masks both.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) /*a*//*b*/ RETURN n"),
        None
    );
}

#[test]
fn readonly_gate_handles_non_executable_regions_at_index_zero() {
    // The arm guards are `i + 1 < b.len()`; several arithmetic mutants on them
    // differ ONLY when the region starts at index 0, because that is the single
    // position where `i - 1` underflows and `i * 1` coincides with `i`. Every
    // other test here has the literal or comment mid-query, so those mutants
    // survived until this case existed.
    // # source: exhaustive differential run, 2026-08-07 (/tmp/exh200b.rs).

    // Leading closed block comment is masked -> the keyword inside is data.
    assert_eq!(
        forbidden_cypher_keyword("/* CREATE (x) */ MATCH (n) RETURN n"),
        None
    );
    // Leading line comment likewise.
    assert_eq!(
        forbidden_cypher_keyword("// DELETE n\nMATCH (n) RETURN n"),
        None
    );
    // Leading string literal likewise.
    assert_eq!(forbidden_cypher_keyword("'load' MATCH (n) RETURN n"), None);
    // A leading comment must not hide a real clause after it.
    assert_eq!(
        forbidden_cypher_keyword("/* x */ MATCH (n) DELETE n"),
        Some("DELETE")
    );
    // Unterminated at index 0 still fails closed.
    assert_eq!(
        forbidden_cypher_keyword("/* never closed"),
        Some("UNTERMINATED_LITERAL")
    );
    assert_eq!(
        forbidden_cypher_keyword("'unclosed load"),
        Some("UNTERMINATED_LITERAL")
    );
    // The whole query being nothing but a closed comment is still a clean read.
    assert_eq!(forbidden_cypher_keyword("/**/"), None);
}

#[test]
fn readonly_gate_fails_closed_on_unterminated_literal() {
    // An unterminated literal would let everything after it escape the scan,
    // so it is refused outright rather than masked to end-of-input.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (s) WHERE s.name = 'oops DELETE s"),
        Some("UNTERMINATED_LITERAL")
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) /* never closed CREATE (m)"),
        Some("UNTERMINATED_LITERAL")
    );
    // ...and the end-to-end path reports it as a policy rejection.
    let args = json!({
        "graph_path": "/nonexistent/graph",
        "query": "MATCH (s) WHERE s.name = 'oops DELETE s"
    });
    let err = do_query_graph(&args).expect_err("must reject unterminated literal");
    assert!(err.contains("read_only_query_required"), "got: {err}");
}

// ---------------------------------------------------------------------------
// B.2 re-audit / B.3 allowlist / B.5 mask-awareness
// ---------------------------------------------------------------------------

/// 2026-08-25 mechanical re-audit of the filesystem denylist against lbug
/// 0.19.1's own headers. `visitDetachDatabase` and `visitUseDatabase` are base
/// no-ops (parsed_statement_visitor.h:60-61) exactly like the four already
/// listed, so the engine classifies both as read-only — and the lexical list
/// stopped at ATTACH, letting them through BOTH gates.
#[test]
fn readonly_gate_blocks_detach_and_use_database() {
    for query in [
        "DETACH DATABASE other",
        "USE DATABASE other",
        "detach database other",
    ] {
        assert!(
            forbidden_cypher_keyword(query).is_some(),
            "must be refused: {query}"
        );
    }
    // `DETACH DELETE` was already caught through DELETE; it stays caught.
    assert!(forbidden_cypher_keyword("MATCH (n) DETACH DELETE n").is_some());
}

/// Schema introspection is reachable through `query_graph`: the two catalog
/// readers are admitted by NAME.
#[test]
fn readonly_gate_admits_allowlisted_introspection_procedures() {
    for query in [
        "CALL table_info('Function') RETURN *",
        "CALL TABLE_INFO('Function') RETURN *",
        "CALL show_tables() RETURN *",
    ] {
        assert_eq!(
            forbidden_cypher_keyword(query),
            None,
            "must be admitted: {query}"
        );
    }
}

/// Everything else a `CALL` can reach stays refused — including the
/// configuration form, which lbug's own analyzer reports as read-only
/// (`visitStandaloneCall` -> readOnly = true), so this lexical layer is the
/// only barrier that exists against it.
#[test]
fn readonly_gate_refuses_unlisted_procedures_and_config_calls() {
    for query in [
        "CALL threads = 8",
        "CALL storage_info('Function') RETURN *",
        "CALL show_connection('x') RETURN *",
        "CALL",
        "MATCH (n) CALL something_else() RETURN n",
    ] {
        assert!(
            forbidden_cypher_keyword(query).is_some(),
            "must be refused: {query}"
        );
    }
}

/// A procedure name reached through `.` or `:` is an identifier, not a call —
/// the same exemption every other keyword gets.
#[test]
fn readonly_gate_allows_call_shaped_identifiers() {
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n:Function) RETURN n.call_count"),
        None
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) WHERE n.name = 'CALL threads = 8' RETURN n"),
        None,
        "a procedure name inside a literal is data, not a call"
    );
}

/// Documents what adding DETACH and USE to the denylist COSTS, so the
/// narrowing is a known quantity rather than a surprise in a bug report.
///
/// Both are whole-word matches on executable text, so the ordinary ways these
/// letters appear in a read query are unaffected: inside a literal, as a
/// property, as a label, or inside a relationship-table name. What IS newly
/// refused is a bare `use`/`detach` in executable position — most plausibly a
/// pattern VARIABLE named `use`. That shape is rare, the workaround is a
/// rename or backticks, and the alternative was leaving two statements that
/// slip both the lexical gate and the engine's own read-only classifier.
#[test]
fn detach_and_use_narrowing_spares_identifier_positions() {
    for still_allowed in [
        // In a literal — masked before the scan.
        "MATCH (n:Function) WHERE n.name = 'use' RETURN n",
        "MATCH (n) WHERE n.doc = 'detach the volume' RETURN n",
        // As a property or a label — identifier positions.
        "MATCH (n:Function) RETURN n.use",
        "MATCH (n:Use) RETURN n",
        // Inside a relationship-table name this schema actually declares.
        "MATCH (f:Function)-[:Uses_Function_Struct]->(s:Struct) RETURN s.id",
        // In a comment.
        "MATCH (n) RETURN n // detach database later",
    ] {
        assert_eq!(
            forbidden_cypher_keyword(still_allowed),
            None,
            "must remain allowed: {still_allowed}"
        );
    }

    // The cost, stated explicitly: a bare identifier spelled like the keyword.
    assert!(
        forbidden_cypher_keyword("MATCH (use:Function) RETURN use").is_some(),
        "a pattern variable named `use` is refused — the accepted cost of \
         closing the DETACH/USE gap"
    );
    // Backticking it is the workaround, and it works because backticks mask.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (`use`:Function) RETURN `use`"),
        None
    );
}
