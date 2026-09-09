//! Issue #284 (lot 5). Drives `resolve_with_client` against a fake healthy
//! LSP server (`tests/fixtures/lsp/fake_server_definition_logger.py`) over a
//! two-file fixture: one file inside the crate's only compiled target
//! (`src/lib.rs`), one file (`kani/h.rs`) that is not. Proves the
//! outside-target skip in `drive_pass` never issues a `didOpen`/`definition`
//! request for the excluded file, while the included file still goes through
//! the normal request path — the distinction the plain-count fixture in
//! `health_gate_tests.rs` cannot make (it never opens ANY file).

use super::*;
use crate::graph_store::{GraphStore, NODE_CALL_SITE, NODE_FUNCTION};
use crate::indexer::cargo_targets;
use crate::lsp_client::{self, LspClient};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A real Cargo crate (one target, `src/lib.rs`) plus `kani/h.rs`, a file the
/// crate's own manifest never compiles — the exact shape of dy-wcet's
/// `kani/response_bounds.rs` gap this lot closes. Both files carry one
/// caller `Function` and one unresolved `CallSite`.
struct OutsideTargetsFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: GraphStore,
}

fn insert_caller_and_site(store: &GraphStore, file: &str, name: &str) {
    let fn_id = format!("{file}::{name}");
    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", &format!("'{fn_id}'")),
                ("name", &format!("'{name}'")),
                ("qualified_name", &format!("'{fn_id}'")),
                ("start_line", "1"),
                ("end_line", "1"),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .unwrap_or_else(|e| panic!("insert Function {fn_id}: {e}"));
    store
        .insert_node(
            NODE_CALL_SITE,
            &[
                ("id", &format!("'{fn_id}::call@1:1'")),
                ("callee_name", "'helper'"),
                ("line", "1"),
                ("col", "1"),
                ("is_resolved", "false"),
                ("language", "'rust'"),
            ],
        )
        .unwrap_or_else(|e| panic!("insert CallSite for {fn_id}: {e}"));
}

fn fixture() -> OutsideTargetsFixture {
    let dir = tempfile::Builder::new()
        .prefix("lsp_outside_targets")
        .tempdir()
        .expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::create_dir_all(root.join("kani")).expect("mkdir kani");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"outside_targets_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(root.join("src/lib.rs"), "pub fn inside() {}\n").expect("write src/lib.rs");
    std::fs::write(root.join("kani/h.rs"), "fn harness() {}\n").expect("write kani/h.rs");

    let store = GraphStore::open_or_create(&root.join("db")).expect("open");
    store.create_schema().expect("schema");
    insert_caller_and_site(&store, "src/lib.rs", "inside");
    insert_caller_and_site(&store, "kani/h.rs", "harness");

    OutsideTargetsFixture {
        _dir: dir,
        root,
        store,
    }
}

/// Spawns the healthy definition-logging fake server (`fake_server_definition_
/// logger.py`) rooted at `root`, logging to `log_path`. Mirrors
/// `health_gate_tests::spawn_fake`, split out for the same reason: keeps the
/// test body under the §4.2 function-length cap.
fn spawn_fake_definition_logger(root: &Path, log_path: &Path) -> LspClient {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/lsp/fake_server_definition_logger.py"
    );
    LspClient::start_unchecked(
        "python3",
        &[script, log_path.to_str().expect("utf8 path")],
        root,
        Duration::from_secs(10),
    )
    .expect("spawn fake server")
}

/// The outside-target file must appear in `events` under NEITHER a `did_open`
/// nor a `definition_requested` line for `uri` — the whole point of the skip.
fn assert_never_touched(events: &str, uri: &str) {
    assert!(
        !events.contains(&format!("did_open:{uri}")),
        "the outside-target file must never be opened: {events}"
    );
    assert!(
        !events.contains(&format!("definition_requested:{uri}")),
        "the outside-target file must never get a definition request: {events}"
    );
}

/// The in-target file must go through the normal request path: both a
/// `did_open` and a `definition_requested` line for `uri`.
fn assert_normally_processed(events: &str, uri: &str) {
    assert!(
        events.contains(&format!("did_open:{uri}")),
        "the in-target file must still be opened normally: {events}"
    );
    assert!(
        events.contains(&format!("definition_requested:{uri}")),
        "the in-target file must still get its definition request: {events}"
    );
}

#[test]
fn a_site_outside_the_compiled_targets_never_gets_a_definition_request() {
    if !lsp_client::is_command_available("python3") {
        eprintln!("skipping: python3 not on PATH (fake LSP server fixture needs it)");
        return;
    }
    if !lsp_client::is_command_available("cargo") {
        eprintln!("skipping: cargo not on PATH (cargo_targets::discover needs it)");
        return;
    }
    let fx = fixture();
    let unresolved = collect_unresolved_callsites(&fx.store).expect("collect");
    assert_eq!(unresolved.len(), 2, "one site inside, one outside");

    let target_map = cargo_targets::discover(&fx.root);
    assert!(
        target_map.is_outside_targets(Path::new("kani/h.rs")),
        "the fixture's own Cargo.toml must not compile kani/h.rs — got {target_map:?}"
    );
    assert!(
        !target_map.is_outside_targets(Path::new("src/lib.rs")),
        "the fixture's own Cargo.toml must compile src/lib.rs — got {target_map:?}"
    );

    let log = tempfile::Builder::new()
        .prefix("lsp_outside_targets_log")
        .tempfile()
        .expect("tempfile");
    let mut client = spawn_fake_definition_logger(&fx.root, log.path());

    let node_index = build_node_position_index(&fx.store).expect("index");
    let plan = PassPlan {
        codebase_path: &fx.root,
        language: "rust",
        deadline: Duration::from_secs(5),
        start: Instant::now(),
        ctx: edges::SiteContext {
            node_index: &node_index,
            canonical_root: &fx.root,
        },
        target_map: &target_map,
    };

    let pass = resolve_with_client(&fx.store, &mut client, &plan, &unresolved)
        .expect("a healthy server must not be gated");
    let result = pass.into_result(0, client.server_health().clone());
    let _ = client.shutdown();

    assert_eq!(
        result.outside_targets_count, 1,
        "exactly the kani/h.rs site must be attributed outside targets"
    );

    let events = std::fs::read_to_string(log.path()).expect("read log");
    assert_never_touched(&events, &lsp_client::path_to_file_uri(&fx.root.join("kani/h.rs")));
    assert_normally_processed(&events, &lsp_client::path_to_file_uri(&fx.root.join("src/lib.rs")));
}
