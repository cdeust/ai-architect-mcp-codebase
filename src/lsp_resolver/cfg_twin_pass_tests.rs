//! Issue #366, part A. Drives the real language-server pass (`resolve_with_client`
//! on a started client) against a scripted fake server that resolves one site
//! to a `#[cfg]` twin the default build compiles out and another to the twin it
//! compiles. The first must get no row and keep the reason `cfg_twins`; the
//! second is recorded. `lsp_command_override` cannot name the fake server (the
//! allowlist refuses any path), so the client is started unchecked, as in
//! `health_gate_tests` and `unlinked_tests`.

use super::*;
use crate::graph_store::{GraphStore, CFG_ACTIVE, CFG_INACTIVE, NODE_CALL_SITE, NODE_FUNCTION};
use crate::lsp_client::LspClient;
use crate::resolver::cfg_verdict::TwinView;
use std::path::Path;
use std::time::{Duration, Instant};

const FAST: &str = "src/lib.rs::pick#cfg(feature=fast)";
const SLOW: &str = "src/lib.rs::pick#cfg(not(feature=fast))";
const TO_FAST: &str = "src/a.rs::caller::call@3:5";
const TO_SLOW: &str = "src/a.rs::caller::call@4:5";

fn function(store: &GraphStore, id: &str, start: u64) {
    let name = crate::graph_store::strip_cfg_gates(id)
        .rsplit("::")
        .next()
        .unwrap_or(id)
        .to_string();
    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", &format!("'{id}'")),
                ("name", &format!("'{name}'")),
                ("qualified_name", &format!("'{id}'")),
                ("start_line", &start.to_string()),
                ("end_line", &(start + 2).to_string()),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .expect("insert function");
}

fn site(store: &GraphStore, id: &str, line: u64) {
    store
        .insert_node(
            NODE_CALL_SITE,
            &[
                ("id", &format!("'{id}'")),
                ("callee_name", "'pick'"),
                ("line", &line.to_string()),
                ("col", "5"),
                ("is_resolved", "false"),
                ("language", "'rust'"),
            ],
        )
        .expect("insert site");
}

/// Two twins of `pick` in src/lib.rs (the default build compiles `SLOW`), a
/// caller in src/a.rs, and one open call site aimed at each twin.
fn twin_store(root: &Path) -> GraphStore {
    std::fs::create_dir_all(root.join("src")).expect("mkdir");
    std::fs::write(
        root.join("src/lib.rs"),
        "#[cfg(feature = \"fast\")]\npub fn pick() {}\n\n#[cfg(not(feature = \"fast\"))]\npub fn pick() {}\n",
    )
    .expect("write lib");
    std::fs::write(
        root.join("src/a.rs"),
        "pub fn caller() {\n\n    pick();\n    pick();\n}\n",
    )
    .expect("write a");
    let store = GraphStore::open_or_create(&root.join("db")).expect("open");
    store.create_schema().expect("schema");
    function(&store, FAST, 2);
    function(&store, SLOW, 5);
    function(&store, "src/a.rs::caller", 1);
    site(&store, TO_FAST, 3);
    site(&store, TO_SLOW, 4);
    store
        .write_cfg_active(&[
            ("Function".to_string(), FAST.to_string(), CFG_INACTIVE),
            ("Function".to_string(), SLOW.to_string(), CFG_ACTIVE),
        ])
        .expect("cfg_active");
    store
}

/// `{"src/a.rs:<0-based line>": definition}` for the two sites.
fn write_script(root: &Path, path: &Path) {
    let lib = crate::lsp_client::path_to_file_uri(&root.join("src/lib.rs"));
    let script = serde_json::json!({
        "src/a.rs:2": {"uri": lib, "line": 1},
        "src/a.rs:3": {"uri": lib, "line": 4},
    });
    std::fs::write(path, script.to_string()).expect("write script");
}

fn rows_from(store: &GraphStore, site: &str) -> Vec<String> {
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite {{id: '{site}'}})-[r:Calls_CallSite_Function]->(t) RETURN t.id"
        ))
        .expect("query")
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect()
}

fn state_of(store: &GraphStore, site: &str) -> Vec<String> {
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite {{id: '{site}'}}) RETURN cs.is_resolved, cs.unresolved_reason"
        ))
        .expect("query")
        .rows
        .into_iter()
        .next()
        .expect("site exists")
}

#[test]
fn a_definition_in_a_compiled_out_twin_gets_no_row_and_keeps_the_twin_reason() {
    if !crate::lsp_client::is_command_available("python3") {
        eprintln!("skipping: python3 not on PATH (fake LSP server fixture needs it)");
        return;
    }
    let dir = tempfile::Builder::new()
        .prefix("lsp_cfg_twin_pass")
        .tempdir()
        .expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
    let store = twin_store(&root);
    let script = root.join("script.json");
    write_script(&root, &script);
    run_scripted_pass(&root, &store, &script, &root.join("server.log"));

    assert!(
        rows_from(&store, TO_FAST).is_empty(),
        "a definition in a twin the build compiles out must not be recorded"
    );
    assert_eq!(state_of(&store, TO_FAST), ["false", "cfg_twins"]);
    assert_eq!(rows_from(&store, TO_SLOW), [SLOW]);
    assert_eq!(state_of(&store, TO_SLOW)[0], "true");
}

/// Runs the real pass over every open site of `store`, answered by the fake
/// server from `script`, and writes its end-of-pass marks.
fn run_scripted_pass(root: &Path, store: &GraphStore, script: &Path, log: &Path) {
    let unresolved = collect_unresolved_callsites(store).expect("collect");
    assert_eq!(unresolved.len(), 2, "both sites start open");
    let node_index = build_node_position_index(store).expect("index");
    let twins = TwinView::load(store);
    let target_map = crate::indexer::cargo_targets::TargetMap::Unknown {
        detail: "test fixture".into(),
    };
    let plan = PassPlan {
        codebase_path: root,
        language: "rust",
        deadline: Duration::from_secs(20),
        start: Instant::now(),
        ctx: edges::SiteContext {
            node_index: &node_index,
            canonical_root: root,
            twins: &twins,
        },
        target_map: &target_map,
    };
    let server = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/lsp/fake_server_scripted_definition.py"
    );
    let mut client = LspClient::start_unchecked(
        "python3",
        &[
            server,
            log.to_str().expect("utf8"),
            script.to_str().expect("utf8"),
        ],
        root,
        Duration::from_secs(10),
    )
    .expect("spawn fake server");
    let pass = resolve_with_client(store, &mut client, &plan, &unresolved).expect("healthy");
    let _ = client.shutdown();
    pass.mark_resolved(store).expect("mark");
}
