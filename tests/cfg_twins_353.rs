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

/// A caller under the SAME gate as one twin still gets no edge in this PR: the
/// choice needs the build's predicate values, which the next PR reads.
#[test]
fn a_caller_under_the_same_gate_as_one_twin_still_gets_no_edge() {
    let source = format!(
        "{TWINS}\n#[cfg(feature = \"fast\")]\npub fn fast_caller() -> u32 {{\n    pick()\n}}\n"
    );
    let tmp = tempfile::tempdir().unwrap();
    let store = index_and_resolve(tmp.path(), &[("lib.rs", &source)]);
    let per_site = rows(
        &store,
        "MATCH (c:CallSite)-[r:Calls_CallSite_Function]->(f:Function) WHERE f.name = 'pick' RETURN c.line",
    );
    assert!(per_site.is_empty(), "{per_site:?}");
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
