//! Issue #282. Drives `resolve_with_client` against the fake `error` server
//! (`tests/fixtures/lsp/fake_server_status_error_server.py`, started via
//! `LspClient::start_unchecked` — the point of this test is the health
//! gate, not the command allowlist `LspClient::start` also checks) to prove
//! the gate stops the pass BEFORE any `textDocument/definition` request, not
//! merely that the final counts happen to come back empty.

use super::*;
use crate::graph_store::{GraphStore, NODE_CALL_SITE, NODE_FUNCTION};
use crate::lsp_client::{LspClient, ServerHealthLevel};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A store with one caller `Function` and one unresolved `CallSite` on it —
/// enough for `collect_unresolved_callsites` to return a non-empty list, so
/// the health gate (not the empty-unresolved early return) is what these
/// tests exercise.
fn store_with_one_unresolved_site(root: &Path) -> GraphStore {
    let store = GraphStore::open_or_create(&root.join("db")).expect("open");
    store.create_schema().expect("schema");
    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", "'src/a.rs::caller'"),
                ("name", "'caller'"),
                ("qualified_name", "'src/a.rs::caller'"),
                ("start_line", "1"),
                ("end_line", "3"),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .expect("insert caller");
    store
        .insert_node(
            NODE_CALL_SITE,
            &[
                ("id", "'src/a.rs::caller::call@1:1'"),
                ("callee_name", "'x'"),
                ("line", "1"),
                ("col", "1"),
                ("is_resolved", "false"),
                ("language", "'rust'"),
            ],
        )
        .expect("insert site");
    store
}

/// A tempdir root carrying `src/a.rs` and the one-unresolved-site store both
/// tests need — grouped so each test body reads as its own gate assertion,
/// not repeated fixture plumbing.
struct GateFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: GraphStore,
}

fn gate_fixture(prefix: &str) -> GateFixture {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
    std::fs::create_dir_all(root.join("src")).expect("mkdir");
    std::fs::write(root.join("src/a.rs"), "fn caller() {}").expect("write");
    let store = store_with_one_unresolved_site(&root);
    GateFixture {
        _dir: dir,
        root,
        store,
    }
}

/// Spawns `script` (a fake LSP server, unchecked — the allowlist is not what
/// these tests exercise) rooted at `root`, logging to `log_path`.
fn spawn_fake(script: &str, log_path: &Path, root: &Path) -> LspClient {
    LspClient::start_unchecked(
        "python3",
        &[script, log_path.to_str().expect("utf8 path")],
        root,
        Duration::from_secs(10),
    )
    .expect("spawn fake server")
}

/// A minimal `PassPlan` over `fixture`'s root and `node_index` — a bare
/// deadline/budget is enough since these tests assert on the health gate,
/// never on `drive_pass`'s per-site behavior.
fn plan_for<'a>(
    fixture: &'a GateFixture,
    node_index: &'a std::collections::HashMap<(String, u64), sites::NodePosition>,
    deadline: Duration,
) -> PassPlan<'a> {
    PassPlan {
        codebase_path: &fixture.root,
        language: "rust",
        deadline,
        start: Instant::now(),
        ctx: edges::SiteContext {
            node_index,
            canonical_root: &fixture.root,
        },
    }
}

#[test]
fn resolve_with_client_fails_before_issuing_a_definition_request() {
    if !crate::lsp_client::is_command_available("python3") {
        eprintln!("skipping: python3 not on PATH (fake LSP server fixture needs it)");
        return;
    }
    let fixture = gate_fixture("lsp_health_gate");
    let unresolved = collect_unresolved_callsites(&fixture.store).expect("collect");
    assert_eq!(unresolved.len(), 1, "the fixture must have one open site");

    let log = tempfile::Builder::new()
        .prefix("lsp_health_gate_log")
        .tempfile()
        .expect("tempfile");
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/lsp/fake_server_status_error_server.py"
    );
    let mut client = spawn_fake(script, log.path(), &fixture.root);

    let node_index = build_node_position_index(&fixture.store).expect("index");
    let plan = plan_for(&fixture, &node_index, Duration::from_secs(5));

    // `LspPass` carries no `Debug` impl (nothing else needs one), so drop the
    // `Ok` payload before `expect_err` — its bound is on `T`, not on `E`.
    let err = resolve_with_client(&fixture.store, &mut client, &plan, &unresolved)
        .map(|_pass| ())
        .expect_err("a server reporting health:error must fail the pass");
    assert!(
        err.starts_with("lsp_workspace_load_failed:"),
        "error must carry the reason code: {err}"
    );
    assert!(
        err.contains("Failed to load workspaces."),
        "the server's own message must survive to the caller: {err}"
    );
    assert_eq!(client.server_health().level, ServerHealthLevel::Error);

    let events = std::fs::read_to_string(log.path()).expect("read log");
    assert!(
        !events.contains("definition_requested"),
        "the health gate must stop the pass before any definition request: {events}"
    );
}

/// A healthy server (the existing `fake_server_status_server.py` fixture,
/// which never sends an `error` health) must NOT be gated — the pass
/// proceeds and returns `Ok`, even though this fixture never answers a
/// `textDocument/definition` request either (it has no capability to), so
/// the site simply goes unresolved rather than the whole pass failing.
#[test]
fn a_healthy_server_is_not_gated() {
    if !crate::lsp_client::is_command_available("python3") {
        eprintln!("skipping: python3 not on PATH (fake LSP server fixture needs it)");
        return;
    }
    let fixture = gate_fixture("lsp_health_gate_ok");
    let unresolved = collect_unresolved_callsites(&fixture.store).expect("collect");

    let log = tempfile::Builder::new()
        .prefix("lsp_health_gate_ok_log")
        .tempfile()
        .expect("tempfile");
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/lsp/fake_server_status_server.py"
    );
    let mut client = spawn_fake(script, log.path(), &fixture.root);

    let node_index = build_node_position_index(&fixture.store).expect("index");
    let plan = plan_for(&fixture, &node_index, Duration::from_secs(2));

    resolve_with_client(&fixture.store, &mut client, &plan, &unresolved)
        .expect("a healthy server must not be gated");
    assert_eq!(client.server_health().level, ServerHealthLevel::Ok);
}
