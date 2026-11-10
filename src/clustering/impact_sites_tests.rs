// Issue #445: `get_impact` names the open call sites that name the target
// (file and line), not only their count, so an agent can read the callers the
// resolver could not bind.

use super::*;
use crate::graph_store::{cypher_str, GraphStore, NODE_CALL_SITE, NODE_FUNCTION};

fn empty_store() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("impact_sites_test")
        .tempdir()
        .expect("create temp dir");
    let store = GraphStore::open_or_create(&dir.path().join("testdb")).expect("open_or_create");
    store.create_schema().expect("create_schema");
    (dir, store)
}

fn insert_function(store: &GraphStore, qn: &str) {
    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", &cypher_str(qn)),
                ("name", &cypher_str(qn)),
                ("qualified_name", &cypher_str(qn)),
                ("start_line", "1"),
                ("end_line", "1"),
                ("visibility", &cypher_str("pub")),
                ("is_async", "false"),
                ("language", &cypher_str("rust")),
            ],
        )
        .expect("insert function node");
}

/// An open site at `line` of `file`, with the id the parsers emit
/// (`<caller_qn>::call@<line>:<col>#<seq>`).
fn insert_open_site(store: &GraphStore, file: &str, line: u32, callee_name: &str) -> String {
    let id = format!("{file}::main::call@{line}:5#0");
    store
        .insert_node(
            NODE_CALL_SITE,
            &[
                ("id", &cypher_str(&id)),
                ("callee_name", &cypher_str(callee_name)),
                ("line", &line.to_string()),
                ("col", "5"),
                ("is_resolved", "false"),
                ("language", &cypher_str("rust")),
            ],
        )
        .expect("insert unresolved CallSite node");
    id
}

#[test]
fn get_impact_names_the_file_and_line_of_an_unresolved_caller() {
    let (_dir, store) = empty_store();
    let target = "src/tasks.rs::TaskSet::response_of";
    insert_function(&store, target);
    let id = insert_open_site(&store, "src/sched.rs", 42, "s.response_of");
    insert_open_site(&store, "src/sched.rs", 7, "s.deadline_of"); // another symbol

    let result = get_impact(&store, target).expect("get_impact");

    assert!(result.callers.is_empty());
    assert_eq!(result.unresolved_callsites_naming_target, 1);
    // A site whose reason cell was never written counts as not recorded, not
    // under the engine's NULL rendering.
    assert_eq!(
        result.unresolved_callsites_by_reason,
        [("not_recorded".to_string(), 1)].into_iter().collect()
    );
    assert_eq!(
        result.unresolved_callsites,
        vec![impact_sites::UnresolvedSite {
            id,
            file: "src/sched.rs".to_string(),
            line: 42,
            callee_name: "s.response_of".to_string(),
            reason: crate::graph_store::callsite_reasons::REASON_NOT_RECORDED.to_string(),
        }]
    );
}

#[test]
fn get_impact_lists_sites_by_file_then_line_and_caps_the_list() {
    let (_dir, store) = empty_store();
    let target = "src/tasks.rs::TaskSet::response_of";
    insert_function(&store, target);
    let total = impact_sites::UNRESOLVED_SITES_LISTED as u32 + 10;
    for i in 0..total {
        let file = if i % 2 == 0 { "src/b.rs" } else { "src/a.rs" };
        insert_open_site(&store, file, total - i, "response_of");
    }

    let result = get_impact(&store, target).expect("get_impact");

    assert_eq!(result.unresolved_callsites_naming_target, u64::from(total));
    assert_eq!(
        result.unresolved_callsites.len(),
        impact_sites::UNRESOLVED_SITES_LISTED
    );
    let keys: Vec<(&str, i64)> = result
        .unresolved_callsites
        .iter()
        .map(|s| (s.file.as_str(), s.line))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "sites must come by file, then line");
    assert_eq!(keys.first(), Some(&("src/a.rs", 1)));
}

#[test]
fn get_impact_lists_no_site_when_none_names_the_target() {
    let (_dir, store) = empty_store();
    let target = "src/tasks.rs::TaskSet::response_of";
    insert_function(&store, target);

    let result = get_impact(&store, target).expect("get_impact");

    assert!(result.unresolved_callsites.is_empty());
}
