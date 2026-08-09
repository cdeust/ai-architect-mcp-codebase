use super::*;
use crate::{handle_tool_call, tool_profile, tools_list, ToolProfile};
use std::fs;

#[test]
fn limit_injection_appends_when_absent() {
    let (q, injected) = inject_limit_if_absent("MATCH (n) RETURN n");
    assert!(injected);
    assert_eq!(
        q,
        format!("MATCH (n) RETURN n LIMIT {QUERY_GRAPH_ROW_LIMIT}")
    );
}

#[test]
fn limit_injection_strips_trailing_semicolon() {
    let (q, injected) = inject_limit_if_absent("MATCH (n) RETURN n;");
    assert!(injected);
    assert_eq!(
        q,
        format!("MATCH (n) RETURN n LIMIT {QUERY_GRAPH_ROW_LIMIT}")
    );
}

#[test]
fn limit_injection_respects_existing_limit() {
    let (q, injected) = inject_limit_if_absent("MATCH (n) RETURN n LIMIT 5");
    assert!(!injected);
    assert_eq!(q, "MATCH (n) RETURN n LIMIT 5");
    // Case-insensitive.
    let (q2, injected2) = inject_limit_if_absent("MATCH (n) RETURN n limit 5");
    assert!(!injected2);
    assert_eq!(q2, "MATCH (n) RETURN n limit 5");
}

#[test]
fn limit_word_boundary_not_fooled_by_identifier() {
    // `node_limit` is an identifier, not a LIMIT clause → still inject.
    let (_q, injected) = inject_limit_if_absent("MATCH (n) RETURN n.node_limit");
    assert!(injected);
    // A real LIMIT after an identifier-named field is still detected.
    assert!(has_limit_clause("MATCH (n) RETURN n.node_limit LIMIT 3"));
}

#[test]
fn test_query_graph_rejects_delete() {
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) DETACH DELETE n"),
        Some("DELETE")
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) DELETE n"),
        Some("DELETE")
    );
    assert_eq!(forbidden_cypher_keyword("CREATE (n:Foo)"), Some("CREATE"));
    assert_eq!(
        forbidden_cypher_keyword("MERGE (n:Foo {id: 1})"),
        Some("MERGE")
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) SET n.x = 1"),
        Some("SET")
    );
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) REMOVE n:Label"),
        Some("REMOVE")
    );
    assert_eq!(forbidden_cypher_keyword("DROP TABLE Foo"), Some("DROP"));
    assert_eq!(forbidden_cypher_keyword("CALL db.labels()"), Some("CALL"));
    assert_eq!(forbidden_cypher_keyword("LOAD CSV FROM 'x'"), Some("LOAD"));

    // Clean read queries must pass.
    assert_eq!(forbidden_cypher_keyword("MATCH (n) RETURN n"), None);
    assert_eq!(
        forbidden_cypher_keyword("OPTIONAL MATCH (n) RETURN n"),
        None
    );
    assert_eq!(forbidden_cypher_keyword("WITH 1 AS x RETURN x"), None);
    assert_eq!(forbidden_cypher_keyword("UNWIND [1,2] AS i RETURN i"), None);

    // Whole-word matching — identifiers that embed a keyword must NOT trigger.
    assert_eq!(
        forbidden_cypher_keyword("MATCH (n) RETURN n.created_at"),
        None
    );
    assert_eq!(forbidden_cypher_keyword("MATCH (n) RETURN n.setting"), None);

    // Case insensitivity.
    assert_eq!(
        forbidden_cypher_keyword("match (n) detach delete n"),
        Some("DELETE")
    );

    // End-to-end via do_query_graph — no real DB needed because we should
    // fail before `GraphStore::open_or_create`.
    let args = json!({
        "graph_path": "/nonexistent/graph",
        "query": "MATCH (n) DETACH DELETE n"
    });
    let err = do_query_graph(&args).expect_err("must reject mutation query");
    assert!(err.contains("read_only_query_required"), "got: {err}");
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

#[test]
fn order_by_detection_whole_word_and_case_insensitive() {
    assert!(has_order_by_clause("MATCH (n) RETURN n ORDER BY n.id"));
    assert!(has_order_by_clause("match (n) return n order by n.name"));
    assert!(has_order_by_clause("MATCH (n) RETURN n ORDER   BY n.id")); // extra ws
                                                                        // No ORDER BY → unstable order.
    assert!(!has_order_by_clause("MATCH (n) RETURN n"));
    assert!(!has_order_by_clause("MATCH (n) RETURN n LIMIT 5"));
    // "order" without "by" is not an ORDER BY clause.
    assert!(!has_order_by_clause("MATCH (n) RETURN n.order"));
    // Identifier embedding 'order' must not trigger.
    assert!(!has_order_by_clause("MATCH (n) RETURN n.reorder_by"));
    // 'by' glued to an identifier must not satisfy the clause.
    assert!(!has_order_by_clause("MATCH (n) RETURN n order byzantine"));
}

#[test]
fn test_health_check_tool_count_matches_tools_list() {
    // source: C-correctness bug 3 — the health_check response must derive
    // the count from `tools_list()` dynamically. If a new tool is added
    // to `tool_schemas::tools_list` without touching main.rs, the count
    // must still be correct.
    let tools = tools_list(ToolProfile::Full);
    let expected = tools
        .get("tools")
        .and_then(|v| v.as_array())
        .expect("tools_list must return a `tools` array")
        .len();

    let health = handle_tool_call(
        &json!({
            "name": "health_check",
            "arguments": {}
        }),
        ToolProfile::Full,
    );

    // handle_tool_call wraps the payload in a content/text envelope.
    // Find the JSON text inside.
    let text = health
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("text"))
        .and_then(|v| v.as_str())
        .expect("health_check must return content[0].text");
    let payload: serde_json::Value =
        serde_json::from_str(text).expect("content[0].text must be JSON");

    let reported = payload
        .get("tools_count")
        .and_then(|v| v.as_u64())
        .expect("tools_count field must be present");
    assert_eq!(reported as usize, expected, "tools_count drift");

    let legacy = payload
        .get("stages_registered")
        .and_then(|v| v.as_u64())
        .expect("stages_registered field must stay for back-compat");
    assert_eq!(legacy as usize, expected, "stages_registered drift");
}

#[test]
fn test_health_check_count_reflects_core_profile() {
    // health_check derives its count from the ACTIVE profile's registry,
    // so a core-profile server reports the core tool count, not 24.
    let health = handle_tool_call(
        &json!({
            "name": "health_check",
            "arguments": {}
        }),
        ToolProfile::Core,
    );
    let text = health
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("text"))
        .and_then(|v| v.as_str())
        .expect("health_check must return content[0].text");
    let payload: serde_json::Value =
        serde_json::from_str(text).expect("content[0].text must be JSON");
    assert_eq!(
        payload.get("tools_count").and_then(|v| v.as_u64()),
        Some(tool_profile::CORE_TOOL_NAMES.len() as u64),
        "core profile must report exactly the core tool count"
    );
}

#[test]
fn test_core_profile_rejects_unregistered_tool_call() {
    // A tool hidden by the profile must be indistinguishable from a tool
    // that does not exist — callable-but-unlisted would defeat the point.
    let response = handle_tool_call(
        &json!({
            "name": "prepare_prd_input",
            "arguments": {}
        }),
        ToolProfile::Core,
    );
    assert_eq!(response.get("isError"), Some(&json!(true)));
    let text = response
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("text"))
        .and_then(|v| v.as_str())
        .expect("rejection must carry content[0].text");
    assert!(text.contains("Unknown tool"), "got: {text}");
    assert!(text.contains("'core' profile"), "got: {text}");
}

#[test]
fn test_graph_path_must_end_in_graph() {
    // source: H4 fix — caller-chosen path is safe ONLY when it is absolute
    // AND the last segment is exactly `graph` AND the path is not one of
    // the forbidden system roots.
    assert!(validate_graph_path_safe(Path::new("/tmp/foo/graph")).is_ok());
    assert!(validate_graph_path_safe(Path::new("/Users/alice/proj/graph")).is_ok());

    // Not absolute.
    assert!(validate_graph_path_safe(Path::new("relative/graph")).is_err());

    // Does not end in /graph.
    assert!(validate_graph_path_safe(Path::new("/etc")).is_err());
    assert!(validate_graph_path_safe(Path::new("/tmp")).is_err());
    assert!(validate_graph_path_safe(Path::new("/")).is_err());
    assert!(validate_graph_path_safe(Path::new("/Users")).is_err());
    assert!(validate_graph_path_safe(Path::new("/tmp/foo/notgraph")).is_err());

    // Ends in /graph but IS a forbidden system root (should still reject).
    assert!(validate_graph_path_safe(Path::new("/etc/graph")).is_err());
    assert!(
        validate_graph_path_safe(Path::new("//graph")).is_err()
            || validate_graph_path_safe(Path::new("//graph")).is_ok()
    );
}

#[test]
fn remove_stale_graph_artifact_handles_file_and_dir() {
    // source: ENOTDIR fix — a prior run can leave `graph` as a single-file
    // Kuzu db; `remove_dir_all` on a file returns ENOTDIR (os error 20).
    // The helper must delete both shapes and report a missing path as an
    // error rather than panicking.
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    use crate::test_support::TempDirExt;
    let base = tempfile::Builder::new()
        .prefix("ap-remove-stale-")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let graph = base.join("graph");

    // Case A: graph is a directory with nested content.
    fs::create_dir_all(graph.join("nested")).unwrap();
    fs::write(graph.join("nested/f.txt"), b"x").unwrap();
    assert!(graph.is_dir());
    remove_stale_graph_artifact(&graph).expect("dir removal");
    assert!(!graph.exists());

    // Case B: graph is a single file — the ENOTDIR regression case.
    fs::write(&graph, b"kuzu-single-file-db").unwrap();
    assert!(graph.is_file());
    remove_stale_graph_artifact(&graph).expect("file removal (was ENOTDIR)");
    assert!(!graph.exists());

    // Missing path → surfaced as an error, never a panic.
    assert!(remove_stale_graph_artifact(&graph).is_err());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn write_graph_meta_records_absolute_root() {
    // The sidecar records the ABSOLUTE indexed root so a consumer can
    // rebuild absolute paths from AP's relative ones (cortex-viz wiki->file
    // join + tool-file keying). It is written NEXT TO the graph, never
    // inside it — the graph itself stays portable.
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    use crate::test_support::TempDirExt;
    let base = tempfile::Builder::new()
        .prefix("ap-meta-")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let root = base.join("some/repo/root");

    write_graph_meta(&base, &root);

    let meta_path = base.join("meta.json");
    assert!(meta_path.is_file(), "meta.json must be written");
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();
    assert_eq!(
        parsed.get("root").and_then(|v| v.as_str()),
        Some(root.to_string_lossy().as_ref()),
        "sidecar must record the absolute root verbatim",
    );
    assert_eq!(
        parsed.get("schema_version").and_then(|v| v.as_u64()),
        Some(1),
    );
    let _ = fs::remove_dir_all(&base);
}
