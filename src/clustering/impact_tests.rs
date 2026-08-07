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
use crate::graph_store::{
    cypher_str, GraphStore, NODE_COMMUNITY, NODE_FILE, NODE_FUNCTION, NODE_PROCESS,
};

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

fn insert_file(store: &GraphStore, id: &str) {
    store
        .insert_node(
            NODE_FILE,
            &[
                ("id", &cypher_str(id)),
                ("path", &cypher_str(id)),
                ("name", &cypher_str(id)),
                (
                    "extension",
                    &cypher_str(id.rsplit('.').next().unwrap_or("")),
                ),
                ("size_bytes", "1"),
                ("parse_errors", "0"),
            ],
        )
        .expect("insert file node");
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

// ---------------------------------------------------------------------------
// File-level reference fan-in (issue #205)
// ---------------------------------------------------------------------------

/// A File target with `References_File_File` edges pointing at it (the shape
/// `light_link`'s markdown/shell extraction produces) surfaces those
/// referrers under `references`, split out from `importers`, and the impact
/// boundary stays `Exact` — the graph has reference-edge indexing (there IS a
/// References_File_File edge), so there is nothing to be blind to.
#[test]
fn get_impact_reports_file_reference_fan_in_as_exact() {
    let (_dir, store) = empty_store();
    let hub = "rules/coding-standards.md";
    let referrer_a = "CONTRIBUTING.md";
    let referrer_b = "tools/foo.sh";

    insert_file(&store, hub);
    insert_file(&store, referrer_a);
    insert_file(&store, referrer_b);
    store
        .insert_edge(
            "References_File_File",
            referrer_a,
            hub,
            &[("confidence", "1.0"), ("resolution_method", "'md-link'")],
        )
        .expect("insert md reference edge");
    store
        .insert_edge(
            "References_File_File",
            referrer_b,
            hub,
            &[("confidence", "1.0"), ("resolution_method", "'sh-source'")],
        )
        .expect("insert sh reference edge");

    let result = get_impact(&store, hub).expect("get_impact");

    assert_eq!(
        result.references.len(),
        2,
        "both referrers must surface under references, got {:?}",
        result.references.iter().map(|n| &n.id).collect::<Vec<_>>()
    );
    assert!(
        result.importers.is_empty(),
        "reference edges must not leak into the code-dependency `importers` section"
    );
    assert_eq!(
        result.epistemic,
        Boundary::Exact,
        "reference edges exist in the graph ⇒ code-only fan-in is not flagged as unindexed"
    );
}

/// A File target with ZERO inbound references, in a graph that contains
/// markdown/shell files but NO `References_File_File` edges anywhere, must be
/// flagged `LowerBound` with the "predates reference-edge indexing" reason —
/// this graph cannot tell "genuinely unreferenced" apart from "built before
/// issue #205 shipped", so it must say so rather than claim completeness.
#[test]
fn get_impact_flags_unindexed_reference_graph_as_lower_bound() {
    let (_dir, store) = empty_store();
    let target = "rules/coding-standards.md";
    let unrelated_md = "README.md"; // present, but never linked via References_

    insert_file(&store, target);
    insert_file(&store, unrelated_md);

    let result = get_impact(&store, target).expect("get_impact");

    assert!(result.references.is_empty());
    assert_eq!(
        result.epistemic,
        Boundary::LowerBound,
        "md/sh files exist but zero References_File_File edges anywhere ⇒ lower bound"
    );
    assert!(
        result
            .epistemic_reasons
            .iter()
            .any(|r| r.contains("References_File_File") && r.contains("lower bound")),
        "must name the unindexed-reference carrier, got {:?}",
        result.epistemic_reasons
    );
}

/// A File target with zero inbound references in a graph where OTHER files
/// DO carry References_File_File edges (reference-edge indexing has clearly
/// run) must NOT be flagged lower-bound on that basis — this is a genuinely
/// unreferenced file, not a coverage gap.
#[test]
fn get_impact_does_not_flag_orphan_file_when_graph_has_reference_indexing() {
    let (_dir, store) = empty_store();
    let target = "docs/orphan.md";
    let other_hub = "rules/coding-standards.md";
    let other_referrer = "CONTRIBUTING.md";

    insert_file(&store, target);
    insert_file(&store, other_hub);
    insert_file(&store, other_referrer);
    store
        .insert_edge(
            "References_File_File",
            other_referrer,
            other_hub,
            &[("confidence", "1.0"), ("resolution_method", "'md-link'")],
        )
        .expect("insert reference edge elsewhere in the graph");

    let result = get_impact(&store, target).expect("get_impact");

    assert!(result.references.is_empty());
    assert_eq!(
        result.epistemic,
        Boundary::Exact,
        "reference-edge indexing ran elsewhere in this graph ⇒ an orphan file is not a coverage gap"
    );
    assert!(result.epistemic_reasons.is_empty());
}
