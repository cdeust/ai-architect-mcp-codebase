//! Issue #353: two items of one name under mutually exclusive `#[cfg]` predicates
//! are two nodes, and a call to that name is never resolved to one of them.
//!
//! Before the fix the graph kept ONE `pick` node (the first in source order) and
//! every call to `pick` resolved to it as `unique-match` at 0.95, whichever twin
//! the build compiles. After it, both twins are nodes named by their gate, the
//! call site is left open with the reason `cfg_twins`, and an old graph that has
//! already collapsed twins is refused for an incremental refresh.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::fs;
use std::path::Path;

const TWINS: &str = "#[cfg(feature = \"fast\")]\n\
pub fn pick() -> u32 {\n    1\n}\n\n\
#[cfg(not(feature = \"fast\"))]\n\
pub fn pick() -> u32 {\n    2\n}\n\n\
pub fn caller() -> u32 {\n    pick()\n}\n";

fn index_and_resolve(tmp: &Path, files: &[(&str, &str)]) -> GraphStore {
    let source = tmp.join("source");
    fs::create_dir_all(&source).expect("mkdir source");
    for (name, text) in files {
        fs::write(source.join(name), text).expect("write fixture");
    }
    let graph = tmp.join("graph");
    indexer::index_codebase(&source, &graph).expect("index");
    let store = GraphStore::open_or_create(&graph).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve");
    store
}

/// Every relationship whose target is a `pick` function, any table, any kind:
/// (table, confidence, method). Structural edges (`Defines`) are direct-AST facts
/// and are not calls; nothing else may reach a twin.
fn edges_into_pick(store: &GraphStore) -> Vec<Vec<String>> {
    rows(
        store,
        "MATCH ()-[r]->(f:Function) WHERE f.name = 'pick' RETURN label(r), r.confidence, r.resolution_method",
    )
    .into_iter()
    .filter(|r| !r[0].starts_with("Defines_"))
    .collect()
}

fn rows(store: &GraphStore, cypher: &str) -> Vec<Vec<String>> {
    store.execute_query(cypher).expect(cypher).rows
}

/// The issue's reproduction: both twins are Function nodes, told apart by gate.
#[test]
fn twin_functions_are_both_in_the_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let store = index_and_resolve(tmp.path(), &[("lib.rs", TWINS)]);
    let mut nodes = rows(
        &store,
        "MATCH (f:Function) WHERE f.name = 'pick' RETURN f.qualified_name, f.cfg_gate, f.start_line ORDER BY f.start_line",
    );
    nodes.sort();
    assert_eq!(nodes.len(), 2, "{nodes:?}");
    assert!(
        nodes.iter().any(|r| r[1] == "feature=fast" && r[2] == "2"),
        "{nodes:?}"
    );
    assert!(
        nodes
            .iter()
            .any(|r| r[1] == "not(feature=fast)" && r[2] == "7"),
        "{nodes:?}"
    );
    assert!(nodes.iter().all(|r| r[0].contains("#cfg(")), "{nodes:?}");
}

/// The false edge of the issue: no per-site row and no function-level edge from
/// `caller` to either twin, whichever one the build compiles.
#[test]
fn a_call_to_twins_gets_no_edge_to_either() {
    let tmp = tempfile::tempdir().unwrap();
    let store = index_and_resolve(tmp.path(), &[("lib.rs", TWINS)]);
    let per_site = rows(
        &store,
        "MATCH (c:CallSite)-[r:Calls_CallSite_Function]->(f:Function) WHERE f.name = 'pick' RETURN c.line, f.qualified_name, r.confidence",
    );
    assert!(per_site.is_empty(), "a twin was chosen: {per_site:?}");
    let edges = rows(
        &store,
        "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) WHERE b.name = 'pick' RETURN a.name, b.qualified_name",
    );
    assert!(edges.is_empty(), "a twin was chosen: {edges:?}");
    let any = edges_into_pick(&store);
    assert!(
        any.is_empty(),
        "an edge of some kind reaches a twin: {any:?}"
    );
}

/// The site stays open, says why, and is not counted resolved.
#[test]
fn the_call_site_is_left_open_with_the_reason_cfg_twins() {
    let tmp = tempfile::tempdir().unwrap();
    let store = index_and_resolve(tmp.path(), &[("lib.rs", TWINS)]);
    let site = rows(
        &store,
        "MATCH (c:CallSite) WHERE c.callee_name = 'pick' RETURN c.is_resolved, c.unresolved_reason",
    );
    assert_eq!(site.len(), 1, "{site:?}");
    assert_eq!(site[0][0].to_lowercase(), "false", "{site:?}");
    assert_eq!(site[0][1], "cfg_twins", "{site:?}");
}

/// Callers that are twins under the same gates as the twins they call each reach
/// the twin of their own gate, whatever the build: the gate in a caller's id is
/// the evidence, and this project has no `Cargo.toml`, so the build decides
/// nothing. The plain `caller` decides nothing and stays open.
#[test]
fn callers_that_are_twins_reach_the_twin_of_their_own_gate() {
    let source = format!(
        "{TWINS}\n#[cfg(feature = \"fast\")]\npub fn run() -> u32 {{\n    pick()\n}}\n\
         #[cfg(not(feature = \"fast\"))]\npub fn run() -> u32 {{\n    pick()\n}}\n"
    );
    let tmp = tempfile::tempdir().unwrap();
    let store = index_and_resolve(tmp.path(), &[("lib.rs", &source)]);
    let mut edges = rows(
        &store,
        "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) WHERE b.name = 'pick' \
         RETURN a.qualified_name, b.qualified_name, r.resolution_method, r.confidence",
    );
    edges.sort();
    assert_eq!(edges.len(), 2, "{edges:?}");
    for edge in &edges {
        assert_eq!(edge[2], "cfg-selected", "{edges:?}");
        assert_eq!(edge[3], "0.85", "{edges:?}");
        let gate = |qn: &str| qn.split_once("#cfg(").unwrap().1.to_string();
        assert_eq!(
            gate(&edge[0]),
            gate(&edge[1]),
            "a caller reached the other twin: {edges:?}"
        );
    }
    let open = rows(
        &store,
        "MATCH (c:CallSite) WHERE c.callee_name = 'pick' AND c.is_resolved = false \
         RETURN c.unresolved_reason",
    );
    assert_eq!(open, [["cfg_twins"]], "{open:?}");
    let above_09 = rows(
        &store,
        "MATCH ()-[r]->(f:Function) WHERE f.name = 'pick' AND r.confidence > 0.9 \
         AND r.resolution_method <> 'direct-ast' RETURN f.id",
    );
    assert!(
        above_09.is_empty(),
        "a twin got more than 0.9: {above_09:?}"
    );
}

/// Control for the two checks above: with ONE `pick` the same queries do see the
/// 0.95 edge, so an empty result on twins is not an empty query.
#[test]
fn the_edge_queries_see_the_edge_when_there_is_one_pick() {
    let tmp = tempfile::tempdir().unwrap();
    let store = index_and_resolve(
        tmp.path(),
        &[(
            "lib.rs",
            "pub fn pick() -> u32 {\n    1\n}\npub fn caller() -> u32 {\n    pick()\n}\n",
        )],
    );
    let any = edges_into_pick(&store);
    assert!(!any.is_empty(), "the query sees no edge at all");
    let at_095 = rows(
        &store,
        "MATCH ()-[r]->(f:Function) WHERE f.name = 'pick' AND r.confidence >= 0.95 AND r.resolution_method <> 'direct-ast' RETURN f.id",
    );
    assert!(!at_095.is_empty(), "no 0.95 edge seen: {any:?}");
}

/// The accounting of a `cfg_twins` site: it is one unresolved reference with the
/// reason, counted once in `total_refs`, and never in `calls_resolved`.
#[test]
fn a_cfg_twins_site_is_counted_once_as_unresolved() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("lib.rs"), TWINS).unwrap();
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&source, &graph).expect("index");
    let store = GraphStore::open_or_create(&graph).expect("open");
    let result = resolver::resolve_graph(&store).expect("resolve");
    assert_eq!(result.calls_resolved, 0);
    let twin_refs: Vec<_> = result
        .unresolved
        .iter()
        .filter(|u| u.reason.starts_with("cfg_twins"))
        .collect();
    assert_eq!(twin_refs.len(), 1, "{} unresolved", result.unresolved.len());
    assert_eq!(twin_refs[0].target_text, "pick");
    assert!(twin_refs[0].reason.contains("2 candidates"));
    let again = resolver::resolve_graph(&store).expect("second resolve");
    assert_eq!(
        again.total_refs, result.total_refs,
        "a second pass recounts"
    );
}

/// Regression pin: an item without a twin keeps its plain id and its 0.95 edge,
/// beside a twin pair in the same file.
#[test]
fn an_item_without_a_twin_resolves_as_before() {
    let source = format!("{TWINS}\npub fn helper() -> u32 {{\n    3\n}}\npub fn user() -> u32 {{\n    helper()\n}}\n");
    let tmp = tempfile::tempdir().unwrap();
    let store = index_and_resolve(tmp.path(), &[("lib.rs", &source)]);
    let edge = rows(
        &store,
        "MATCH (c:CallSite)-[r:Calls_CallSite_Function]->(f:Function) WHERE f.name = 'helper' RETURN f.qualified_name, r.resolution_method, r.confidence",
    );
    assert_eq!(edge.len(), 1, "{edge:?}");
    assert!(
        edge[0][0].ends_with("::helper") && !edge[0][0].contains("#cfg("),
        "{edge:?}"
    );
    assert_eq!(edge[0][1], "unique-match", "{edge:?}");
}

/// Twins in DIFFERENT files are not a collision: each file has one `pick`, both
/// keep their plain ids, and the call from a third file is an ordinary ambiguity
/// (dropped, reason "ambiguous"), not a `cfg_twins` one. Deferred to a later PR.
#[test]
fn twins_in_different_files_are_an_ordinary_ambiguity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = index_and_resolve(
        tmp.path(),
        &[
            ("a.rs", "#[cfg(unix)]\npub fn pick() -> u32 {\n    1\n}\n"),
            (
                "b.rs",
                "#[cfg(not(unix))]\npub fn pick() -> u32 {\n    2\n}\n",
            ),
            ("c.rs", "pub fn caller() -> u32 {\n    pick()\n}\n"),
        ],
    );
    let picks = rows(
        &store,
        "MATCH (f:Function) WHERE f.name = 'pick' RETURN f.qualified_name",
    );
    assert_eq!(picks.len(), 2, "{picks:?}");
    assert!(picks.iter().all(|r| !r[0].contains("#cfg(")), "{picks:?}");
    let site = rows(
        &store,
        "MATCH (c:CallSite) WHERE c.callee_name = 'pick' RETURN c.unresolved_reason",
    );
    assert_eq!(site.len(), 1);
    assert_ne!(site[0][0], "cfg_twins", "{site:?}");
}

/// A graph written before the fix has already collapsed its twins, and adding an
/// empty column cannot split them: an incremental refresh is refused and asks
/// for a full reindex, as it does for a graph without entry attributes.
#[test]
fn a_graph_without_the_gate_column_requires_a_full_reindex() {
    let tmp = tempfile::tempdir().unwrap();
    let store = index_and_resolve(tmp.path(), &[("lib.rs", TWINS)]);
    store
        .require_cfg_gate_metadata()
        .expect("a fresh graph carries the column");
    store
        .execute_query("ALTER TABLE Function DROP cfg_gate")
        .expect("drop the column to imitate an old graph");
    let refused = store
        .require_cfg_gate_metadata()
        .expect_err("an old graph is refused");
    assert!(refused.contains("full reindex required"), "{refused}");
}
