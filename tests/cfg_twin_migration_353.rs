//! Issue #353, review round: a graph indexed before this change has already
//! collapsed its twins, and an empty column cannot split them.
//!
//! (a) an incremental refresh of such a graph is refused with the reindex
//! message; (b) a full reindex over the same output directory rebuilds the
//! tables so the twins split and `cfg_gate` is populated; (c) the library entry
//! point says what to do instead of failing at the first write; (d) nothing in
//! the sources adds the column with an `ALTER`, which is the rule that makes the
//! column's presence a proof of twin identity.
use ai_architect_mcp::{
    graph_store::{GraphStore, CFG_GATE_LABELS},
    indexer,
};
use std::fs;
use std::path::Path;

const ONE: &str = "#[cfg(unix)]\npub fn pick() {}\n";
const TWO: &str = "#[cfg(unix)]\npub fn pick() {}\n#[cfg(not(unix))]\npub fn pick() {}\n";

fn index_over_stdio(source: &Path, output: &Path) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_ai-architect-mcp-codebase"))
        .args(["--profile", "full"])
        .env_remove("AP_PROFILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{
        "name":"index_codebase", "arguments":{"path":source,"output_dir":output,"cochange":false}
    }});
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let text = envelope["result"]["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(response["status"], "ok", "{envelope}");
}

/// The shape an older build wrote: no `cfg_gate` column on any table.
fn make_old_shape(graph: &Path) {
    let store = GraphStore::open_or_create(graph).unwrap();
    for label in CFG_GATE_LABELS {
        store
            .execute_query(&format!("ALTER TABLE {label} DROP cfg_gate"))
            .unwrap();
    }
    assert!(!store.node_column_exists("Function", "cfg_gate").unwrap());
}

fn incremental(source: &Path, output: &Path) -> Result<(), String> {
    let manifest_path = indexer::manifest::manifest_path(output);
    let prior = indexer::manifest::load(&manifest_path).expect("manifest");
    indexer::index_incremental(
        source,
        &output.join("graph"),
        &manifest_path,
        &indexer::IndexOptions::default(),
        &prior,
    )
    .map(|_| ())
}

#[test]
fn an_old_graph_is_refused_incrementally_and_a_full_reindex_splits_the_twins() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let output = tmp.path().join("output");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("lib.rs"), ONE).unwrap();
    fs::write(
        source.join("user.rs"),
        "pub fn caller() {\n    pick();\n}\n",
    )
    .unwrap();
    index_over_stdio(&source, &output);
    let graph = output.join("graph");
    // What the old build persisted for two twins: ONE node, the first twin.
    make_old_shape(&graph);
    fs::write(source.join("lib.rs"), TWO).unwrap();

    let refused = incremental(&source, &output).expect_err("an old graph must be refused");
    assert!(refused.contains("full reindex required"), "{refused}");

    // The handler falls back to a full rebuild, which replaces the tables.
    index_over_stdio(&source, &output);
    let store = GraphStore::open_or_create(&graph).unwrap();
    for label in CFG_GATE_LABELS {
        assert!(
            store.node_column_exists(label, "cfg_gate").unwrap(),
            "{label} lacks cfg_gate after the full reindex"
        );
    }
    let mut twins = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'pick' RETURN f.id, f.cfg_gate")
        .unwrap()
        .rows;
    twins.sort();
    assert_eq!(
        twins,
        [
            ["lib.rs::pick#cfg(not(unix))", "not(unix)"],
            ["lib.rs::pick#cfg(unix)", "unix"]
        ]
    );
    drop(store);
    incremental(&source, &output).expect("a graph rebuilt by this build takes refreshes");
}

#[test]
fn the_library_entry_point_names_the_remedy_on_an_old_graph_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("lib.rs"), TWO).unwrap();
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&source, &graph).expect("index");
    make_old_shape(&graph);
    let error = indexer::index_codebase(&source, &graph)
        .err()
        .expect("an old graph directory is not silently written to");
    assert!(error.contains("full reindex required"), "{error}");
}

/// The rule that makes the column a proof: no source line adds `cfg_gate` to an
/// existing table. If one does, an old collapsed graph would pass the guard.
#[test]
fn nothing_adds_the_gate_column_with_an_alter() {
    fn scan(dir: &Path, hits: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = fs::read_to_string(&path).unwrap();
                let add = ["ADD ", "cfg_gate"].concat();
                if text.contains(&add) {
                    hits.push(format!("{}: ALTER .. ADD", path.display()));
                }
                for (at, _) in text.match_indices("ensure_node_column(") {
                    let window: String = text[at..].chars().take(160).collect();
                    if window.contains("cfg_gate") {
                        hits.push(format!("{}: ensure_node_column", path.display()));
                    }
                }
            }
        }
    }
    let mut hits = Vec::new();
    scan(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut hits,
    );
    assert!(hits.is_empty(), "{hits:?}");
}

/// A graph with every `cfg_gate` column, as an earlier commit of this work wrote
/// it, carries no marker or an older one: its twin ids were spelled by an older
/// canonical form. The columns alone must not pass the guard.
fn refused_then_rebuilt(spoil: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let output = tmp.path().join("output");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("lib.rs"), TWO).unwrap();
    index_over_stdio(&source, &output);
    let graph = output.join("graph");
    {
        let store = GraphStore::open_or_create(&graph).unwrap();
        let marker = |s: &GraphStore| {
            s.execute_query("MATCH (m:GraphMarker {id: 'cfg_canonical_form'}) RETURN m.value")
                .unwrap()
                .rows
        };
        // Written by the indexer of this build, the current canonical form.
        assert_eq!(marker(&store), [["2"]], "bump with CANONICAL_FORM_VERSION");
        assert!(store.node_column_exists("Function", "cfg_gate").unwrap());
        store.execute_query(spoil).unwrap();
    }
    let refused = incremental(&source, &output).expect_err("columns alone must not pass");
    assert!(refused.contains("full reindex required"), "{refused}");
    index_over_stdio(&source, &output);
    let store = GraphStore::open_or_create(&graph).unwrap();
    let rows = store
        .execute_query("MATCH (m:GraphMarker {id: 'cfg_canonical_form'}) RETURN m.value")
        .unwrap()
        .rows;
    assert_eq!(rows, [["2"]]);
    assert!(
        store.has_code_context(),
        "the rebuild writes the code-context marker too"
    );
    drop(store);
    incremental(&source, &output).expect("a rebuilt graph takes refreshes");
}

#[test]
fn a_graph_with_the_columns_but_no_marker_is_refused_and_a_full_reindex_fixes_it() {
    refused_then_rebuilt("MATCH (m:GraphMarker) DELETE m");
}

#[test]
fn a_graph_with_an_older_marker_is_refused_and_a_full_reindex_fixes_it() {
    refused_then_rebuilt("MATCH (m:GraphMarker) SET m.value = '1'");
}

/// The marker is written at the END of a successful index: a full index that
/// fails leaves none, so the partial graph is refused by the next incremental
/// refresh instead of passing as current.
#[test]
fn a_failed_full_index_leaves_no_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("lib.rs"), TWO).unwrap();
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&source, &graph).expect("index");
    let marker_rows = |graph: &Path| {
        let store = GraphStore::open_or_create(graph).unwrap();
        store
            .execute_query("MATCH (m:GraphMarker {id: 'cfg_canonical_form'}) RETURN m.value")
            .unwrap()
            .rows
    };
    assert_eq!(
        marker_rows(&graph),
        [["2"]],
        "control: a good index writes it"
    );
    // Re-index the same directory from a codebase path that cannot be walked.
    let missing = tmp.path().join("no-such-dir");
    assert!(indexer::index_codebase(&missing, &graph).is_err());
    assert!(
        marker_rows(&graph).is_empty(),
        "a failed index kept the marker"
    );
    let store = GraphStore::open_or_create(&graph).unwrap();
    assert!(store.require_cfg_gate_metadata().is_err());
}
