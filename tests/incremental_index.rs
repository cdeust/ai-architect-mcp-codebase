// incremental_index — issue #62 end-to-end gate (library level).
//
// This test asserts CORRECTNESS only (a deterministic gate — §13 G3, §6.2):
// after mutating a tree and running an incremental pass, the graph is IDENTICAL
// to a from-scratch full index of the same tree, and the pass touched only the
// changed files. The PERFORMANCE claim ("incremental is much cheaper than full")
// lives in `benchmarks/incremental_speed/` with recorded hardware — NOT here. A
// wall-clock ratio in this gate was machine/profile-dependent (issue #74) and
// took the suite red on fast hardware, so it was removed.
//
// This test:
//   1. Builds a fixture repo,
//   2. Full-indexes it into graph A and writes the file manifest,
//   3. Mutates the tree: edit one code file, edit one imported JS file, add a
//      file, delete a file, rename a file,
//   4. Runs the incremental pass over graph A,
//   5. Full-indexes the MUTATED tree from scratch,
//   6. Asserts the filled graph == the full index query-for-query (parity),
//   7. Asserts the incremental counts partition the change set exactly
//      (touched only the changed files),
//   8. Asserts a cross-file inbound edge into an edited file survived.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, manifest, IncrementalResult, IndexOptions};
use std::fs;
use std::path::{Path, PathBuf};

/// Number of generated Python modules — enough for a meaningful parity check
/// across the edit/add/delete/rename mutations. (The large fixture that used to
/// give a timing margin moved to benchmarks/incremental_speed/.)
const N_MODULES: usize = 20;

/// A generated Python module body — several functions + a class per file, so the
/// parity check exercises real symbols and edges.
fn module_body(i: usize) -> String {
    let mut s = String::new();
    for j in 0..8 {
        s.push_str(&format!(
            "def mod{i}_fn{j}(x):\n    return x + {i} + {j}\n\n"
        ));
    }
    for k in 0..3 {
        s.push_str(&format!(
            "class Widget{i}_{k}:\n    def spin(self):\n        return {i}\n    \
             def flip(self, y):\n        return y\n\n"
        ));
    }
    s
}

/// The fixed battery of read queries used to compare two graphs. Covers node
/// totals, edge totals, per-label symbol sets, and the cross-file light-link
/// edge set — so a divergence in any of them fails the parity assertion.
fn snapshot_queries(store: &GraphStore) -> Vec<(String, Vec<Vec<String>>)> {
    let queries = [
        "MATCH (n) RETURN count(n)",
        "MATCH ()-[r]->() RETURN count(r)",
        "MATCH (f:Function) RETURN f.name ORDER BY f.name",
        "MATCH (m:Method) RETURN m.name ORDER BY m.name",
        "MATCH (c:Struct) RETURN c.name ORDER BY c.name",
        "MATCH (f:File) RETURN f.id ORDER BY f.id",
        "MATCH (d:Directory) RETURN d.id ORDER BY d.id",
        "MATCH (a:File)-[:Imports_File_File]->(b:File) RETURN a.id, b.id ORDER BY a.id, b.id",
        "MATCH (df:File)-[:Defines_File_Function]->(fn:Function) \
         RETURN df.id, fn.name ORDER BY df.id, fn.name",
    ];
    queries
        .iter()
        .map(|q| {
            let qr = store
                .execute_query(q)
                .unwrap_or_else(|e| panic!("query failed [{q}]: {e}"));
            (q.to_string(), qr.rows)
        })
        .collect()
}

fn write_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("mk src");
    fs::create_dir_all(root.join("js")).expect("mk js");
    for i in 0..N_MODULES {
        fs::write(root.join(format!("src/mod{i:03}.py")), module_body(i)).expect("write module");
    }
    // A JS import pair: app.js (unchanged in the mutation) imports util.js
    // (edited in the mutation). Exercises inbound-edge preservation: the edge
    // app.js -> util.js must survive editing util.js.
    fs::write(
        root.join("js/app.js"),
        "import { u } from './util.js';\nexport const run = () => u;\n",
    )
    .expect("write app.js");
    fs::write(root.join("js/util.js"), "export const u = 1;\n").expect("write util.js");
}

/// Steps (a)-(e) of the fixture mutation: edit a code file, edit the imported
/// JS file (app.js stays untouched), add a file, delete a file, rename a
/// file. Exercises every `IncrementalResult` classification in one pass.
fn mutate_tree(repo: &Path) {
    fs::write(
        repo.join("src/mod000.py"),
        "def brand_new_fn():\n    return 999\n\ndef another_new():\n    return 0\n",
    )
    .expect("edit mod000");
    fs::write(repo.join("js/util.js"), "export const u = 42;\n").expect("edit util.js");
    fs::write(
        repo.join("src/mod_added.py"),
        "def added_symbol():\n    return 1\n",
    )
    .expect("add file");
    fs::remove_file(repo.join("src/mod001.py")).expect("delete file");
    fs::rename(
        repo.join("src/mod002.py"),
        repo.join("src/mod002_renamed.py"),
    )
    .expect("rename file");
}

/// Assert the `IncrementalResult` counts partition `mutate_tree`'s change set
/// exactly: only the changed+added+renamed-new files are re-parsed, and every
/// untouched module is classified unchanged.
fn assert_incremental_counts(inc: &IncrementalResult) {
    assert_eq!(inc.changed, 2, "edited mod000.py + util.js");
    assert_eq!(inc.added, 1, "mod_added.py");
    assert_eq!(inc.deleted, 1, "mod001.py");
    assert_eq!(inc.renamed, 1, "mod002.py -> mod002_renamed.py");
    assert_eq!(
        inc.files_reparsed, 4,
        "only changed+added+renamed-new files are re-parsed (2+1+1)"
    );
    assert!(
        inc.unchanged >= (N_MODULES as u64) - 3,
        "the untouched files must all be classified unchanged, got {}",
        inc.unchanged
    );
}

/// Assert the inbound cross-file edge app.js -> util.js survived editing its
/// target (util.js).
fn assert_inbound_edge_survived(graph_filled: &Path) {
    let store = GraphStore::open_or_create(graph_filled).expect("open filled graph");
    let imp = store
        .execute_query(
            "MATCH (a:File)-[:Imports_File_File]->(b:File) \
             WHERE a.id = 'js/app.js' RETURN b.id",
        )
        .expect("import query");
    assert!(
        imp.rows.iter().any(|r| r[0] == "js/util.js"),
        "app.js -> util.js must survive editing util.js, got {:?}",
        imp.rows
    );
}

/// Assert the incrementally-filled graph equals a from-scratch full index of
/// the mutated tree, query-for-query.
fn assert_parity(graph_filled: &Path, graph_full: &Path) {
    let snap_a = {
        let store = GraphStore::open_or_create(graph_filled).expect("open filled graph");
        snapshot_queries(&store)
    };
    let snap_b = {
        let store = GraphStore::open_or_create(graph_full).expect("open full graph");
        snapshot_queries(&store)
    };
    assert_eq!(
        snap_a, snap_b,
        "incremental graph must equal a from-scratch full index of the mutated tree"
    );
}

/// Full-indexes `repo` into a fresh graph + manifest under `tmp/out_a`.
/// Returns (graph_a, manifest_a).
fn full_index_with_manifest(tmp: &Path, repo: &Path) -> (PathBuf, PathBuf) {
    let out_a = tmp.join("out_a");
    fs::create_dir_all(&out_a).expect("mk out_a");
    let graph_a = out_a.join("graph");
    let manifest_a = manifest::manifest_path(&out_a);
    indexer::index_codebase_with_language(repo, &graph_a, &IndexOptions::default())
        .expect("full index A");
    indexer::write_full_manifest(repo, &manifest_a, &IndexOptions::default())
        .expect("write manifest A");
    (graph_a, manifest_a)
}

#[test]
fn incremental_matches_full_reindex() {
    let tmp = tempfile::Builder::new()
        .prefix("incremental_index_")
        .tempdir()
        .expect("temp dir");
    let repo = tmp.path().join("repo");
    write_fixture(&repo);

    // -- 2. Full index into graph A + manifest ------------------------------
    let (graph_a, manifest_a) = full_index_with_manifest(tmp.path(), &repo);

    // -- 3. Mutate the tree -------------------------------------------------
    mutate_tree(&repo);

    // -- 4. Incremental pass over graph A (in place) ------------------------
    let prior = manifest::load(&manifest_a).expect("manifest must load");
    let inc = indexer::index_incremental(
        &repo,
        &graph_a,
        &manifest_a,
        &IndexOptions::default(),
        &prior,
    )
    .expect("incremental pass");
    let graph_filled = graph_a.clone();

    // -- 5. Full index of the MUTATED tree from scratch (parity baseline) ---
    let graph_full = tmp.path().join("graph_full");
    indexer::index_codebase_with_language(&repo, &graph_full, &IndexOptions::default())
        .expect("full index");

    // -- 7. Counts partition the change set exactly -------------------------
    assert_incremental_counts(&inc);

    // -- 8. Inbound cross-file edge survived the edit of its target ---------
    assert_inbound_edge_survived(&graph_filled);

    // -- 6. Parity: filled == full, query-for-query -------------------------
    assert_parity(&graph_filled, &graph_full);
}

/// Builds the caller.py/target.py fixture, full-indexes it, and synthesises a
/// resolved cross-file Calls edge caller() -> callee() (the index stage never
/// emits this kind directly — only `resolve_graph` does). Returns
/// (repo, graph, manifest_path).
fn setup_resolved_edge_fixture(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let repo = tmp.join("repo");
    fs::create_dir_all(repo.join("src")).expect("mk src");
    fs::write(
        repo.join("src/caller.py"),
        "def caller():\n    return callee()\n",
    )
    .expect("write caller");
    fs::write(repo.join("src/target.py"), "def callee():\n    return 1\n").expect("write target");

    let out = tmp.join("out");
    fs::create_dir_all(&out).expect("mk out");
    let graph = out.join("graph");
    let manifest_p = manifest::manifest_path(&out);
    indexer::index_codebase_with_language(&repo, &graph, &IndexOptions::default()).expect("index");
    indexer::write_full_manifest(&repo, &manifest_p, &IndexOptions::default()).expect("manifest");

    let store = GraphStore::open_or_create(&graph).expect("open");
    store
        .insert_edge(
            "Calls_Function_Function",
            "src/caller.py::caller",
            "src/target.py::callee",
            &[("confidence", "1.0"), ("resolution_method", "'test'")],
        )
        .expect("insert synthetic cross-file call edge");

    (repo, graph, manifest_p)
}

#[test]
fn incremental_preserves_resolved_cross_file_edges() {
    // Focused proof of the inbound-edge snapshot: a *resolved* cross-file edge
    // (the kind resolve_graph produces, and the kind the C reference's snapshot
    // exists to protect) into an edited file's SYMBOL must survive the purge.
    let tmp = tempfile::Builder::new()
        .prefix("incremental_relink_")
        .tempdir()
        .expect("temp dir");
    let (repo, graph, manifest_p) = setup_resolved_edge_fixture(tmp.path());

    // Edit the TARGET file (keeps callee() so its qn is stable). The purge drops
    // callee and its inbound Calls edge; the snapshot must re-link it.
    fs::write(
        repo.join("src/target.py"),
        "def callee():\n    return 2  # edited\n",
    )
    .expect("edit target");

    let prior = manifest::load(&manifest_p).expect("load manifest");
    let inc =
        indexer::index_incremental(&repo, &graph, &manifest_p, &IndexOptions::default(), &prior)
            .expect("incremental");
    assert_eq!(inc.changed, 1, "only target.py changed");

    let store = GraphStore::open_or_create(&graph).expect("reopen");
    let calls = store
        .execute_query(
            "MATCH (a:Function)-[:Calls_Function_Function]->(b:Function) \
             WHERE a.id = 'src/caller.py::caller' RETURN b.id",
        )
        .expect("calls query");
    assert!(
        calls.rows.iter().any(|r| r[0] == "src/target.py::callee"),
        "the inbound cross-file Calls edge into the edited file must be preserved, got {:?}",
        calls.rows
    );
}
