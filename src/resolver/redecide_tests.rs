// resolver::redecide_tests: issue #443, Claude review of #523. A row an
// incremental refresh decides again carries the confidence and method a full
// index of the same tree writes, not those of the pass that first wrote it.

use super::{apply, carries, changed};
use crate::graph_store::GraphStore;
use crate::resolver::retract::{Decision, Met, RowKey};
use crate::{indexer, resolver};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

fn decision(confidence: f64, method: &str) -> Decision {
    Decision {
        confidence,
        method: method.to_string(),
    }
}

fn key(from: &str, to: &str) -> RowKey {
    (
        "Calls_Function_Function".to_string(),
        from.to_string(),
        to.to_string(),
    )
}

#[test]
fn a_held_row_carries_only_the_same_method_and_confidence() {
    let held = decision(0.9, "unique-global");
    assert!(carries(&held, &decision(0.9, "unique-global")));
    assert!(!carries(&held, &decision(0.9, "import-match")));
    assert!(!carries(&held, &decision(0.95, "unique-global")));
    assert!(!carries(&decision(f64::NAN, "unique-global"), &held));
}

#[test]
fn only_owned_rows_whose_provenance_differs_are_changed() {
    let mut held = HashMap::new();
    held.insert(key("a", "b"), decision(0.9, "unique-global"));
    held.insert(key("a", "c"), decision(0.9, "unique-global"));
    let mut met = Met::new();
    met.insert(key("a", "b"), decision(0.95, "import-match"));
    met.insert(key("a", "c"), decision(0.9, "unique-global"));
    // A row of another writer (not held): never touched.
    met.insert(key("a", "d"), decision(0.95, "import-match"));
    let rows = changed(&held, &met);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, &key("a", "b"));
}

/// Every `Calls_*` row into a `beta`, with its provenance, sorted.
fn provenance(graph: &Path) -> Vec<String> {
    let store = GraphStore::open_or_create(graph).expect("open");
    let mut rows = Vec::new();
    for rel in ["Calls_CallSite_Function", "Calls_Function_Function"] {
        let from = rel.split('_').nth(1).unwrap();
        let cypher = format!(
            "MATCH (a:{from})-[r:{rel}]->(t:Function) WHERE t.name = 'beta' \
             RETURN a.id, t.id, r.resolution_method, r.confidence"
        );
        let qr = store.execute_query(&cypher).expect("provenance");
        rows.extend(
            qr.rows
                .into_iter()
                .map(|r| format!("{rel}: {}", r.join(" | "))),
        );
    }
    rows.sort();
    rows
}

fn resolve(graph: &Path) {
    let store = GraphStore::open_or_create(graph).expect("open");
    resolver::resolve_graph(&store).expect("resolve");
}

fn write(source: &Path, files: &[(&str, &str)]) {
    for (name, text) in files {
        fs::write(source.join(name), text).unwrap();
    }
}

const LIB: &str = "mod a;\nmod b;\nmod c;\n";
/// `alpha` imports `beta` from `b` and calls it (resolved `unique-match`).
const A: &str = "use crate::b::beta;\npub fn alpha() {\n    beta();\n}\n";
/// `alpha` calls the `beta` its own file defines.
const A_PY: &str = "def beta():\n    pass\n\n\ndef alpha():\n    beta()\n";

#[test]
fn a_call_resolved_by_another_tier_after_an_edit_carries_the_fresh_provenance() {
    // `a.py` (unchanged) defines and calls `beta`, unique in the first pass
    // (`unique-match`). An edit to `c.py` declares a second `beta`: the call
    // no longer resolves by uniqueness and still resolves to `a.py::beta`, as
    // the one in the caller's file (`same-file-unique`), so the rows keep
    // their keys and change their provenance.
    let tmp = tempfile::tempdir().unwrap();
    let source: PathBuf = tmp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    write(
        &source,
        &[("a.py", A_PY), ("c.py", "def gamma():\n    pass\n")],
    );
    let graph = tmp.path().join("graph");
    let manifest = tmp.path().join("manifest.json");
    let options = indexer::IndexOptions::default();
    indexer::index_codebase(&source, &graph).expect("index");
    indexer::write_full_manifest(&source, &manifest, &options).expect("manifest");
    resolve(&graph);
    let before = provenance(&graph);
    assert!(!before.is_empty(), "control: the call resolves: {before:?}");

    write(&source, &[("c.py", "def beta():\n    pass\n")]);
    let prior = indexer::manifest::load(&manifest).expect("manifest");
    indexer::index_incremental(&source, &graph, &manifest, &options, &prior)
        .expect("incremental refresh");
    resolve(&graph);

    let fresh_graph = tmp.path().join("fresh");
    indexer::index_codebase(&source, &fresh_graph).expect("fresh index");
    resolve(&fresh_graph);
    let fresh = provenance(&fresh_graph);
    assert_ne!(fresh, before, "the fixture must change the provenance");
    assert_eq!(provenance(&graph), fresh);
}

#[test]
fn an_update_leaves_a_row_of_another_method_between_the_same_nodes_alone() {
    // The `WHERE` on the held method: a row whose method is not the held one
    // is not rewritten.
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    write(
        &source,
        &[
            ("lib.rs", LIB),
            ("a.rs", A),
            ("b.rs", "pub fn beta() {}\n"),
            ("c.rs", "pub fn gamma() {}\n"),
        ],
    );
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&source, &graph).expect("index");
    resolve(&graph);
    let store = GraphStore::open_or_create(&graph).expect("open");
    let row = key("a.rs::alpha", "b.rs::beta");
    let mut held = HashMap::new();
    held.insert(row.clone(), decision(0.5, "not-the-method-of-the-row"));
    let mut met = Met::new();
    met.insert(row, decision(0.1, "import-match"));
    let before = provenance(&graph);
    assert_eq!(apply(&store, &held, &met), Ok(1));
    drop(store);
    assert_eq!(provenance(&graph), before);
}
