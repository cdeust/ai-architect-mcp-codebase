// Tests for `get_impact`'s outside-Cargo-target attribution (issue #284, lot
// 5). Split from `impact_tests.rs` (already at the §4.1 500-line cap before
// this lot) rather than appended to it — a clean concern boundary: general
// impact-aggregation coverage there, this lot's attribution-specific coverage
// here.

use super::*;
use crate::graph_store::{cypher_str, GraphStore, NODE_CALL_SITE, NODE_FUNCTION};

fn empty_store(prefix: &str) -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
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

/// Inserts an unresolved `CallSite` carrying `unresolved_reason` — the shape
/// `LspPass::mark_resolved` leaves behind for a site attributed outside every
/// compiled Cargo target (empty string when `reason` is `None`, matching
/// what the indexer itself writes for a freshly parsed site).
fn insert_unresolved_callsite(store: &GraphStore, id: &str, callee_name: &str, reason: Option<&str>) {
    store
        .insert_node(
            NODE_CALL_SITE,
            &[
                ("id", &cypher_str(id)),
                ("callee_name", &cypher_str(callee_name)),
                ("line", "1"),
                ("col", "1"),
                ("is_resolved", "false"),
                ("language", &cypher_str("rust")),
                ("unresolved_reason", &cypher_str(reason.unwrap_or(""))),
            ],
        )
        .expect("insert unresolved CallSite node");
}

/// Issue #284 (lot 5), plan `tasks/plan-issues-282-283-284.md` §3.4: a
/// `CallSite` with `unresolved_reason = 'outside_compiled_targets'` must
/// surface as `unresolved_callsites_outside_targets == 1`, and the prose
/// reason must name the file it sits in.
#[test]
fn get_impact_attributes_a_callsite_outside_the_compiled_targets() {
    let (_dir, store) = empty_store("impact_outside_targets_attributed");
    let target_qn = "src/lib.rs::TaskSet::response_of";

    insert_function(&store, target_qn);
    insert_unresolved_callsite(
        &store,
        "kani/response_bounds.rs::harness::call@1:1",
        "s.response_of",
        Some(crate::graph_store::CALLSITE_UNRESOLVED_REASON_OUTSIDE_TARGETS),
    );

    let result = get_impact(&store, target_qn).expect("get_impact");

    assert_eq!(result.unresolved_callsites_naming_target, 1);
    assert_eq!(result.unresolved_callsites_outside_targets, 1);
    assert!(
        result
            .epistemic_reasons
            .iter()
            .any(|r| r.contains("kani/response_bounds.rs")),
        "the reason must name the file the outside-target site sits in, got {:?}",
        result.epistemic_reasons
    );
}

/// A `CallSite` with an empty (never-attributed) `unresolved_reason` — the
/// value the indexer itself writes on every freshly parsed site — must not be
/// counted as outside the compiled targets.
#[test]
fn get_impact_does_not_count_an_unattributed_callsite_as_outside_targets() {
    let (_dir, store) = empty_store("impact_outside_targets_unattributed");
    let target_qn = "src/lib.rs::TaskSet::response_of";

    insert_function(&store, target_qn);
    insert_unresolved_callsite(&store, "cs::1", "s.response_of", None);

    let result = get_impact(&store, target_qn).expect("get_impact");

    assert_eq!(result.unresolved_callsites_naming_target, 1);
    assert_eq!(result.unresolved_callsites_outside_targets, 0);
}

/// Issue #284 (lot 5): a graph indexed before `unresolved_reason` existed
/// (the legacy five-column `CallSite` table) must still answer `get_impact`
/// with the plain unresolved count and NO error — never the hard Binder
/// exception a query naming a missing property would otherwise raise
/// (`clustering::impact_reasons::unresolved_callsite_attribution`'s
/// `node_column_exists` gate).
/// A store whose `CallSite`/`Function` tables predate this lot: no
/// `unresolved_reason` column at all (`create_schema` then a manual
/// `CallSite` override would conflict with the PRIMARY KEY constraint on
/// redefinition, so this builds the minimum legacy schema by hand), carrying
/// one `Function` at `target_qn` and one unresolved `CallSite` naming it.
/// Split out of the test body so the test itself stays under the §4.2
/// function-length cap.
fn legacy_store_with_one_unresolved_site(target_qn: &str) -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("impact_outside_targets_legacy_graph")
        .tempdir()
        .expect("create temp dir");
    let store = GraphStore::open_or_create(&dir.path().join("testdb")).expect("open_or_create");
    store
        .execute_query(
            "CREATE NODE TABLE CallSite(id STRING, callee_name STRING, line INT64, \
             col INT64, is_resolved BOOLEAN, language STRING, PRIMARY KEY(id))",
        )
        .expect("legacy CallSite table");
    store
        .execute_query(
            "CREATE NODE TABLE Function(id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, is_async BOOLEAN, \
             language STRING, PRIMARY KEY(id))",
        )
        .expect("Function table");
    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", &cypher_str(target_qn)),
                ("name", &cypher_str(target_qn)),
                ("qualified_name", &cypher_str(target_qn)),
                ("start_line", "1"),
                ("end_line", "1"),
                ("visibility", &cypher_str("pub")),
                ("is_async", "false"),
                ("language", &cypher_str("rust")),
            ],
        )
        .expect("insert function node");
    store
        .insert_node(
            NODE_CALL_SITE,
            &[
                ("id", "'cs::1'"),
                ("callee_name", "'s.response_of'"),
                ("line", "1"),
                ("col", "1"),
                ("is_resolved", "false"),
                ("language", "'rust'"),
            ],
        )
        .expect("insert legacy unresolved CallSite node");
    (dir, store)
}

#[test]
fn get_impact_falls_back_to_a_plain_count_on_a_graph_without_unresolved_reason() {
    let target_qn = "src/lib.rs::TaskSet::response_of";
    let (_dir, store) = legacy_store_with_one_unresolved_site(target_qn);

    let result = get_impact(&store, target_qn).expect("get_impact must not hard-error");

    assert_eq!(
        result.unresolved_callsites_naming_target, 1,
        "the plain count must still work on a legacy graph"
    );
    assert_eq!(
        result.unresolved_callsites_outside_targets, 0,
        "no attribution is possible without the unresolved_reason column"
    );
}
