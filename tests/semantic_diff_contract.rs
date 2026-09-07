//! Audited contracts: status-aware import deltas and canonical evidence.
use ai_architect_mcp::{
    graph_store::GraphStore,
    indexer, resolver,
    semantic_diff::{self, SemanticDiffArgs},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn graph(root: &Path, name: &str, source: &str) -> PathBuf {
    let repo = root.join(format!("{name}-src"));
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/lib.rs"), source).unwrap();
    let path = root.join(name);
    indexer::index_codebase(&repo, &path).unwrap();
    let store = GraphStore::open_or_create(&path).unwrap();
    resolver::resolve_graph(&store).unwrap();
    path
}
fn compare(before: &Path, after: &Path) -> semantic_diff::SemanticDiffOutcome {
    semantic_diff::diff(
        &SemanticDiffArgs {
            before_graph_path: before.into(),
            after_graph_path: after.into(),
        },
        "fixed-time".into(),
    )
    .unwrap()
}
#[test]
fn import_status_changes_are_measured_and_new_unresolved_cannot_be_clean() {
    let root = tempfile::tempdir().unwrap();
    let before = graph(
        root.path(),
        "before",
        "mod a { pub fn helper() {} }\nuse a::helper;\nfn caller() { helper(); }",
    );
    let after = graph(
        root.path(),
        "after",
        "mod a { pub fn renamed() {} }\nuse a::helper;\nfn caller() { helper(); }",
    );
    let broken = compare(&before, &after);
    assert_eq!(broken.summary.new_unresolved_delta, 1);
    assert_eq!(
        broken.regression_score,
        semantic_diff::WEIGHT_UNRESOLVED_DELTA
    );
    assert_eq!(broken.verdict, "concerning");
    let repaired = compare(&after, &before);
    assert_eq!(repaired.summary.new_unresolved_delta, -1);
    assert_eq!(repaired.verdict, "clean");
}
#[test]
fn adding_a_resolved_import_does_not_increase_unresolved_count() {
    let root = tempfile::tempdir().unwrap();
    let before = graph(
        root.path(),
        "before",
        "mod a { pub fn helper() {} }\nfn caller() {}",
    );
    let after = graph(
        root.path(),
        "after",
        "mod a { pub fn helper() {} }\nuse a::helper;\nfn caller() { helper(); }",
    );
    assert_eq!(compare(&before, &after).summary.new_unresolved_delta, 0);
}
#[test]
fn missing_import_schema_is_an_error_not_zero_unresolved() {
    let root = tempfile::tempdir().unwrap();
    let before = graph(root.path(), "before", "fn main() {}");
    let after = graph(root.path(), "after", "fn main() {}");
    {
        let store = GraphStore::open_or_create(&after).unwrap();
        store
            .execute_query("ALTER TABLE Import DROP is_resolved")
            .unwrap();
    }
    assert!(semantic_diff::diff(
        &SemanticDiffArgs {
            before_graph_path: before,
            after_graph_path: after
        },
        "fixed-time".into()
    )
    .is_err());
}
#[test]
fn edge_evidence_is_sorted_before_truncation_and_repeatable() {
    let root = tempfile::tempdir().unwrap();
    // Exercise both a complete detail list and the public truncation boundary.
    for count in [12, semantic_diff::DETAILS_TRUNCATION + 5] {
        let definitions = (0..count)
            .map(|i| format!("fn f{i:03}() {{}}\n"))
            .collect::<String>();
        let calls = (0..count)
            .map(|i| format!("f{i:03}();"))
            .collect::<String>();
        let before = graph(
            root.path(),
            &format!("before-{count}"),
            &format!("{definitions}fn main() {{}}"),
        );
        let after = graph(
            root.path(),
            &format!("after-{count}"),
            &format!("{definitions}fn main() {{{calls}}}"),
        );
        assert_canonical_edges(&before, &after, count);
    }
}

fn assert_canonical_edges(before: &Path, after: &Path, count: usize) {
    let first = compare(before, after);
    assert_eq!(first.summary.edges_added, count as u64);
    let details = first.report["details"]["edges_added"].as_array().unwrap();
    let targets: Vec<_> = details
        .iter()
        .map(|v| v["to"].as_str().unwrap().to_string())
        .collect();
    let expected: Vec<_> = (0..count.min(semantic_diff::DETAILS_TRUNCATION))
        .map(|i| format!("src/lib.rs::f{i:03}"))
        .collect();
    assert_eq!(targets, expected);
    assert_eq!(compare(before, after).report, first.report);
    let removed = compare(after, before);
    assert_eq!(
        removed.report["details"]["edges_removed"],
        first.report["details"]["edges_added"]
    );
}

#[test]
fn semantic_projection_query_failures_are_not_empty_snapshots() {
    let root = tempfile::tempdir().unwrap();
    let before = graph(
        root.path(),
        "before",
        "fn main() { helper(); }\nfn helper() {}",
    );
    for (name, mutation) in [
        ("node", "ALTER TABLE Function DROP qualified_name"),
        ("edge", "DROP TABLE Calls_Function_Function"),
    ] {
        let after = graph(root.path(), name, "fn main() { helper(); }\nfn helper() {}");
        {
            let store = GraphStore::open_or_create(&after).unwrap();
            store.execute_query(mutation).unwrap();
        }
        assert!(
            semantic_diff::diff(
                &SemanticDiffArgs {
                    before_graph_path: before.clone(),
                    after_graph_path: after
                },
                "fixed-time".into()
            )
            .is_err(),
            "{name} query failure must propagate"
        );
    }
}
