use super::*;
use crate::graph_store::NODE_FILE;

// source: review finding #3 on issue #13's fix — file_has_graph_node used
// to build its Cypher literal via a naive single-quote-only
// `String::replace` escape, which a `\'` payload in claim.token
// (LLM-generated PRD content, not trusted)
// can defeat: the escape turns `\'` into `\\'`, an escaped backslash
// followed by an UNescaped closing quote, breaking out of the string
// literal early. Mirrors graph_store::tests::test_cypher_injection_rejected.
#[test]
fn test_file_has_graph_node_escapes_adversarial_path() {
    // GraphStore persists as a single-file embedded db, not a directory
    // (source: main.rs::remove_stale_graph_artifact / the ENOTDIR fix it
    // documents), so the db lives inside a wrapping directory — matching
    // graph_store::tests::test_cypher_injection_rejected.
    //
    // source: issue #21 — a fixed `temp_dir().join("prd_validator_cypher_
    // inject_test")` path collides under default parallel `cargo test`
    // execution. tempfile::TempDir allocates a unique-per-call directory
    // and cleans itself up on drop (mirrors the #13-fix pattern in
    // tests/lbug_bulk_investigation.rs).
    let dir = tempfile::Builder::new()
        .prefix("prd_validator_cypher_inject_test")
        .tempdir()
        .expect("create temp dir");
    let db_path = dir.path().join("testdb");
    let store = GraphStore::open_or_create(&db_path).expect("open_or_create");
    store.create_schema().expect("create_schema");

    // Adversarial path: a literal `\'` sequence followed by a Cypher
    // payload. Under the old naive escape this closes the string early
    // and lets `DETACH DELETE n` execute as live Cypher.
    let evil_path = r"weird\'.rs' DETACH DELETE n //";
    store
        .insert_node(
            NODE_FILE,
            &[
                ("id", &cypher_str(evil_path)),
                ("path", &cypher_str(evil_path)),
                ("name", &cypher_str("evil.rs")),
                ("extension", &cypher_str("rs")),
                ("size_bytes", "0"),
            ],
        )
        .expect("insert adversarial file node");

    // A second, benign node — if the adversarial path's DETACH DELETE
    // had executed, this node would vanish too.
    store
        .insert_node(
            NODE_FILE,
            &[
                ("id", &cypher_str("safe.rs")),
                ("path", &cypher_str("safe.rs")),
                ("name", &cypher_str("safe.rs")),
                ("extension", &cypher_str("rs")),
                ("size_bytes", "0"),
            ],
        )
        .expect("insert safe file node");

    assert!(
        file_has_graph_node(&store, evil_path),
        "the adversarial path must still round-trip as an ordinary string and match its own node"
    );
    assert!(
        file_has_graph_node(&store, "safe.rs"),
        "the benign node must survive — the adversarial path must not have executed DETACH DELETE"
    );
}

// source: PR #487 review. A failed probe must be `None` (-> `Unscored`), never
// "no match" (-> `Absent`): a store with no schema fails every query. Once the
// schema exists, `Some(false)` also proves every CFG_GATE_LABELS table has the
// `name` column the probe reads; a label without one would fail the query.
#[test]
fn test_name_exists_probes_every_label_and_reports_failure() {
    let dir = tempfile::Builder::new()
        .prefix("prd_validator_name_exists")
        .tempdir()
        .expect("create temp dir");
    let store = GraphStore::open_or_create(&dir.path().join("testdb")).expect("open_or_create");
    assert_eq!(
        name_exists(&store, "Anything"),
        None,
        "no tables: probe fails"
    );
    store.create_schema().expect("create_schema");
    assert_eq!(name_exists(&store, "Anything"), Some(false));
}
