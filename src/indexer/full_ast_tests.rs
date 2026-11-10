// indexer full-AST opt-in tests (issue #448).
//
// The full-AST layer (`AstNode` + `FileContent` + their two edge tables) was
// persisted on every index and measured at 94.7 % of django's graph nodes,
// while no typed tool reads it (raw `query_graph` Cypher can). It is now
// opt-in through `IndexOptions::full_ast`.
// These tests pin both sides of that contract on the full index and the
// incremental re-index (on -> off purges, on -> on replaces). The artifact
// fill shares `IndexOptions::parse_policy` and `apply_changes` with the
// incremental path but is not exercised here.

use super::*;
use crate::graph_store::cypher_str;
use crate::test_support::TempDirExt;

fn temp(prefix: &str) -> crate::test_support::TestTempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
        .keep_managed()
}

fn count(store: &GraphStore, label: &str) -> u64 {
    let qr = store
        .execute_query(&format!("MATCH (n:{label}) RETURN count(n)"))
        .expect("count query");
    qr.rows[0][0].parse().expect("count is an integer")
}

fn count_ast_for(store: &GraphStore, rel: &str) -> u64 {
    // Every AstNode row of the file (by `file_id`), not only the root, so
    // orphaned children left by a partial purge would be counted too.
    count_where(
        store,
        "AstNode",
        &format!("n.file_id = {}", cypher_str(rel)),
    )
}

fn count_where(store: &GraphStore, label: &str, predicate: &str) -> u64 {
    let qr = store
        .execute_query(&format!(
            "MATCH (n:{label}) WHERE {predicate} RETURN count(n)"
        ))
        .expect("count query");
    qr.rows[0][0].parse().expect("count is an integer")
}

/// `a.py` after the edit: one more function than the original (`f`, `h`).
const EDITED_A: &str = "def f():\n    return 3\n\n\ndef h():\n    pass\n";

/// `b.py`, never edited by any test.
const B_SRC: &str = "def g():\n    return 2\n";

/// Row count of the full AST `capture_full_ast` produces for `source`: what
/// the store must hold for that file, root included.
fn tree_rows(source: &str, rel: &str) -> u64 {
    crate::parser::ast_capture::capture_full_ast(source, rel, crate::parser::Language::Python)
        .expect("capture")
        .nodes
        .len() as u64
}

/// Edge rows of the file's AST layer: (`Defines_File_AstNode` from the file,
/// `AstChild_AstNode_AstNode` into the file's nodes).
fn ast_edges_for(store: &GraphStore, rel: &str) -> (u64, u64) {
    let q = |cypher: String| -> u64 {
        store.execute_query(&cypher).expect("edge query").rows[0][0]
            .parse()
            .expect("count is an integer")
    };
    let id = cypher_str(rel);
    (
        // File -> its root only (the root is the node with no parent).
        q(format!("MATCH (f:File)-[r:Defines_File_AstNode]->(n:AstNode) WHERE f.id = {id} AND n.parent_id = '' RETURN count(r)")),
        // Parent -> child, and only from the child's recorded parent.
        q(format!("MATCH (p:AstNode)-[r:AstChild_AstNode_AstNode]->(c:AstNode) WHERE c.file_id = {id} AND c.parent_id = p.id RETURN count(r)")),
    )
}

fn write_fixture(root: &Path) {
    std::fs::write(root.join("a.py"), "def f():\n    return 1\n").unwrap();
    std::fs::write(root.join("b.py"), B_SRC).unwrap();
}

fn index(root: &Path, graph: &Path, full_ast: bool) -> GraphStore {
    let options = IndexOptions {
        full_ast,
        ..IndexOptions::default()
    };
    index_codebase_with_language(root, graph, &options).expect("index should succeed");
    GraphStore::open_or_create(graph).expect("open graph")
}

#[test]
fn default_index_persists_no_full_ast_layer() {
    let root = temp("full_ast_default_src_");
    write_fixture(&root);
    let graph = temp("full_ast_default_graph_");
    let _ = std::fs::remove_dir_all(&graph);

    let store = index(&root, &graph, false);

    assert_eq!(
        count(&store, "AstNode"),
        0,
        "AST rows must be opt-in (#448)"
    );
    assert_eq!(
        count(&store, "FileContent"),
        0,
        "source blobs belong to the AST layer"
    );
    assert!(
        count(&store, "Function") >= 2,
        "symbols are indexed either way"
    );
}

#[test]
fn full_ast_option_persists_the_layer_for_every_parsed_file() {
    let root = temp("full_ast_on_src_");
    write_fixture(&root);
    let graph = temp("full_ast_on_graph_");
    let _ = std::fs::remove_dir_all(&graph);

    let store = index(&root, &graph, true);

    assert!(count_ast_for(&store, "a.py") > 0, "a.py has its AST rows");
    assert_eq!(
        count_ast_for(&store, "b.py"),
        tree_rows(B_SRC, "b.py"),
        "b.py has its whole tree"
    );
    let n = tree_rows(B_SRC, "b.py");
    assert_eq!(
        ast_edges_for(&store, "b.py"),
        (1, n - 1),
        "one root edge, one parent edge per other node"
    );
    assert_eq!(
        count(&store, "FileContent"),
        2,
        "one source blob per parsed file"
    );
}

/// Full index with `full_ast: true`, then edit `a.py` and refresh it
/// incrementally with `refresh`. Returns the store after the refresh.
fn index_on_then_refresh_a(tag: &str, refresh: &IndexOptions) -> GraphStore {
    let root = temp(&format!("full_ast_{tag}_src_"));
    write_fixture(&root);
    let out = temp(&format!("full_ast_{tag}_out_"));
    let graph = out.join("graph");
    let manifest_path = manifest::manifest_path(&out);
    let on = IndexOptions {
        full_ast: true,
        ..IndexOptions::default()
    };
    index_codebase_with_language(&root, &graph, &on).expect("full index");
    write_full_manifest(&root, &manifest_path, &on).expect("manifest");
    let prior = manifest::load(&manifest_path).expect("manifest loads");
    std::fs::write(root.join("a.py"), EDITED_A).unwrap();
    index_incremental(&root, &graph, &manifest_path, refresh, &prior).expect("incremental");
    GraphStore::open_or_create(&graph).expect("open graph")
}

fn count_content_for(store: &GraphStore, rel: &str) -> u64 {
    count_where(store, "FileContent", &format!("n.id = {}", cypher_str(rel)))
}

#[test]
fn incremental_refresh_without_full_ast_purges_only_the_reparsed_file() {
    let store = index_on_then_refresh_a("on_off", &IndexOptions::default());

    assert_eq!(
        count_ast_for(&store, "a.py"),
        0,
        "re-parsed file loses its AST"
    );
    assert_eq!(count_content_for(&store, "a.py"), 0, "and its source blob");
    assert_eq!(
        count_ast_for(&store, "b.py"),
        tree_rows(B_SRC, "b.py"),
        "an unchanged file keeps its whole AST"
    );
    assert_eq!(count_content_for(&store, "b.py"), 1, "and its source blob");
}

#[test]
fn incremental_refresh_with_full_ast_replaces_the_reparsed_file_layer() {
    let on = IndexOptions {
        full_ast: true,
        ..IndexOptions::default()
    };
    let store = index_on_then_refresh_a("on_on", &on);

    // `persist_full_ast` logs and swallows insert errors, so a skipped purge
    // would leave the OLD rows in place and a row count alone would still be
    // 1. Only content of the EDITED file proves the layer was replaced.
    let fresh_blob = format!(
        "n.id = {} AND n.original_size = {}",
        cypher_str("a.py"),
        EDITED_A.len()
    );
    assert_eq!(
        count_where(&store, "FileContent", &fresh_blob),
        1,
        "a.py's source blob is the edited source"
    );
    assert_eq!(count_content_for(&store, "a.py"), 1, "and the only one");
    let defs = format!(
        "n.file_id = {} AND n.kind = 'function_definition'",
        cypher_str("a.py")
    );
    assert_eq!(
        count_where(&store, "AstNode", &defs),
        2,
        "a.py's AST is the edited tree (f and h)"
    );
    // The exact row count of the edited tree: independent of how the store
    // treats a duplicate id, and catches stale or orphaned rows.
    let expected = tree_rows(EDITED_A, "a.py");
    assert_eq!(
        count_ast_for(&store, "a.py"),
        expected,
        "a.py holds exactly the edited tree's rows"
    );
    assert_eq!(
        ast_edges_for(&store, "a.py"),
        (1, expected - 1),
        "the edited tree's edges replace the old ones"
    );
    assert_eq!(count_content_for(&store, "b.py"), 1, "b.py is untouched");
    assert_eq!(
        count_ast_for(&store, "b.py"),
        tree_rows(B_SRC, "b.py"),
        "and keeps its whole AST"
    );
}
