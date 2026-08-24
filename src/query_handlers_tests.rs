use super::*;
use crate::{handle_tool_call, tool_profile, tools_list, ToolProfile};

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
