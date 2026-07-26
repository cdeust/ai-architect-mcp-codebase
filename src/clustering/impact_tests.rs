// Tests for `get_impact`'s community/process aggregation loops.
//
// These pin the observable contract of the two `if !row.is_empty()` push
// guards at impact.rs:59 (community aggregation) and :77 (process
// aggregation): a symbol that IS a member of a community / participant in a
// process must surface that membership in the returned `ImpactResult`. Without
// these, the `delete !` mutant on either guard survives — inverting the guard
// to `if row.is_empty()` yields an *empty* aggregate (every real row is
// non-empty, so the inverted guard never pushes), a silent regression no other
// test observed because the existing get_impact tests exercise only the
// reverse-dependency (callers) path, never community/process membership.
// source: issue #84 — kill the two pre-existing get_impact survivors.

use super::*;
use crate::epistemic::Boundary;
use crate::graph_store::{cypher_str, GraphStore, NODE_COMMUNITY, NODE_FUNCTION, NODE_PROCESS};

/// Builds a fresh, schema-initialized store in a unique temp dir. The caller
/// owns the returned `TempDir`; dropping it removes the database.
fn empty_store() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("impact_aggregation_test")
        .tempdir()
        .expect("create temp dir");
    let db_path = dir.path().join("testdb");
    let store = GraphStore::open_or_create(&db_path).expect("open_or_create");
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

/// A function that belongs to a community must have that community's id
/// surface in `communities`. Kills impact.rs:59 `delete !`: the inverted guard
/// pushes only on empty rows (none exist for a 1-column `RETURN c.id`), so the
/// mutant returns an empty `communities` vec while the contract requires the
/// member community.
#[test]
fn get_impact_reports_community_membership() {
    let (_dir, store) = empty_store();
    let fn_qn = "app::svc::checkout";
    let community_id = "community::louvain::1::7";

    insert_function(&store, fn_qn);
    store
        .insert_node(
            NODE_COMMUNITY,
            &[
                ("id", &cypher_str(community_id)),
                ("name", &cypher_str("community_7")),
                ("algorithm", &cypher_str("louvain+c2")),
                ("resolution_param", "1.0"),
                ("member_count", "1"),
                ("modularity_contribution", "0.0"),
            ],
        )
        .expect("insert community node");
    store
        .insert_edge("MemberOf_Function_Community", fn_qn, community_id, &[])
        .expect("insert MemberOf edge");

    let result = get_impact(&store, fn_qn).expect("get_impact");

    // Observable contract: the member community's id is aggregated. The
    // inverted-guard mutant yields `vec![]` here.
    assert_eq!(
        result.communities,
        vec![community_id.to_string()],
        "a function that is MemberOf a community must surface that community's id"
    );
    // No reverse-dependency edges and a concrete (non-Trait) target ⇒ the
    // impact set is exact and carries no heuristic reason. Pins impact.rs:123
    // `heuristic_count > 0` against the `>= 0` / `== 0` mutants, which would
    // spuriously push a heuristic reason and flip the boundary to LowerBound
    // when zero heuristic edges exist.
    assert_eq!(
        result.epistemic,
        Boundary::Exact,
        "no heuristic edges ⇒ the impact boundary is exact"
    );
    assert!(
        result.epistemic_reasons.is_empty(),
        "no heuristic edges ⇒ no epistemic reasons, got {:?}",
        result.epistemic_reasons
    );
}

/// A function that participates in a process must have that process's name
/// surface in `processes`. Kills impact.rs:77 `delete !` by the same argument
/// as the community case, on the process aggregation loop.
#[test]
fn get_impact_reports_process_participation() {
    let (_dir, store) = empty_store();
    let fn_qn = "app::svc::handler";
    // `get_impact`'s process query returns `p.name`; `persist_process_node`
    // sets `name` == the process id, so we assert against the id here.
    let process_id = "process::entry::app::main";

    insert_function(&store, fn_qn);
    store
        .insert_node(
            NODE_PROCESS,
            &[
                ("id", &cypher_str(process_id)),
                ("name", &cypher_str(process_id)),
                ("entry_point_id", &cypher_str("app::main")),
                ("entry_kind", &cypher_str("main")),
                ("entry_confidence", "1.0"),
                ("depth", "1"),
                ("symbol_count", "1"),
            ],
        )
        .expect("insert process node");
    store
        .insert_edge("ParticipatesIn_Function_Process", fn_qn, process_id, &[])
        .expect("insert ParticipatesIn edge");

    let result = get_impact(&store, fn_qn).expect("get_impact");

    // Observable contract: the participated process's name is aggregated. The
    // inverted-guard mutant yields `vec![]` here.
    assert_eq!(
        result.processes,
        vec![process_id.to_string()],
        "a function that ParticipatesIn a process must surface that process's name"
    );
    assert_eq!(
        result.epistemic,
        Boundary::Exact,
        "no heuristic edges ⇒ the impact boundary is exact"
    );
    assert!(
        result.epistemic_reasons.is_empty(),
        "no heuristic edges ⇒ no epistemic reasons, got {:?}",
        result.epistemic_reasons
    );
}

/// A reverse-dependency edge resolved below full confidence must flip the
/// impact boundary to `LowerBound` and emit a heuristic reason. A structural
/// `Calls_` edge carrying no stored `confidence` falls back to the 0.90
/// relation floor (< 1.0), so it counts as heuristic.
///
/// Kills impact.rs:123 `heuristic_count > 0` against the `== 0` and `< 0`
/// mutants: with one heuristic caller the original pushes a reason (LowerBound),
/// while `== 0` and `< 0` both evaluate false → no reason → Exact.
#[test]
fn get_impact_flags_heuristic_edge_as_lower_bound() {
    let (_dir, store) = empty_store();
    let target_qn = "app::svc::target";
    let caller_qn = "app::svc::caller";

    insert_function(&store, target_qn);
    insert_function(&store, caller_qn);
    // Structural Calls edge, no stored confidence ⇒ 0.90 floor ⇒ heuristic.
    store
        .insert_edge("Calls_Function_Function", caller_qn, target_qn, &[])
        .expect("insert Calls edge");

    let result = get_impact(&store, target_qn).expect("get_impact");

    assert_eq!(
        result.callers.len(),
        1,
        "the one heuristic caller must be captured"
    );
    assert_eq!(
        result.epistemic,
        Boundary::LowerBound,
        "a sub-confident (heuristic) reverse-dependency edge ⇒ lower-bound boundary"
    );
    assert!(
        result
            .epistemic_reasons
            .iter()
            .any(|r| r.contains("heuristically")),
        "a heuristic edge must emit the heuristic epistemic reason, got {:?}",
        result.epistemic_reasons
    );
}
