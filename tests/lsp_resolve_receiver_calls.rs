// lsp_resolve_receiver_calls — end-to-end regression for fix/lsp-receiver-calls.
//
// Drives the REAL `lsp_resolve` (Stage 3b-v2) against rust-analyzer to prove
// the three root causes are fixed together: `CallSite.col` is persisted (not
// hardcoded to 0), `textDocument/definition` targets the method identifier
// (not the receiver), and the client waits for rust-analyzer's own
// workDoneProgress before issuing the first request. `#[ignore]`d when
// `rust-analyzer` is not on PATH — the reason is in the ignore string, per
// CONTRIBUTING.md's testing policy.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::lsp_client::is_command_available;
use ai_architect_mcp::{indexer, lsp_resolver, resolver};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

mod common;
use common::{TempDirExt, TestTempDir};

const RESPONSE_OF_QN: &str = "src/lib.rs::TaskSet::response_of";

#[test]
#[ignore = "requires rust-analyzer on PATH; run with `cargo test -- --ignored` \
            once `rustup component add rust-analyzer` or an equivalent install \
            has put it there"]
fn lsp_resolve_binds_receiver_method_calls_via_rust_analyzer() {
    if !is_command_available("rust-analyzer") {
        eprintln!("skipping: rust-analyzer not on PATH");
        return;
    }

    // `_tmp_root` must outlive the whole test: `TestTempDir::drop` deletes
    // its directory on a PASSING test (see src/test_support.rs), so a
    // helper that constructed and returned it would delete the fixture out
    // from under the assertions the instant it returned.
    let (_tmp_root, fixture) = write_receiver_call_fixture();
    let graph_dir = fixture
        .parent()
        .expect("fixture has a parent")
        .join("graph");
    // Index the WHOLE crate root (Cargo.toml alongside src/) so qualified
    // names come out `src/lib.rs::...` — the same root `lsp_resolve` (and
    // rust-analyzer, which needs the Cargo.toml to load the crate) uses.
    indexer::index_codebase(&fixture, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open store");
    resolver::resolve_graph(&store).expect("static resolve");

    assert!(
        callers_of_response_of(&store).is_empty(),
        "the static resolver must NOT already bind this receiver call \
         (if it does, this fixture no longer exercises the LSP path)"
    );

    let result =
        lsp_resolver::resolve_with_lsp(&store, &fixture, "rust", None, Duration::from_secs(30))
            .expect("lsp_resolve");
    assert!(
        result.resolved_count > 0,
        "lsp_resolve must resolve the receiver call: {:?}",
        result.resolved_count
    );

    let after = callers_of_response_of(&store);
    assert_eq!(
        after,
        vec!["src/lib.rs::TaskSet::total".to_string()],
        "response_of must gain exactly one caller — TaskSet::total — via the LSP pass"
    );

    // Soundness regression (PR #267 follow-up): `extra_call_entries`
    // (#87, deliberate) also emits a speculative CallSite for the bare
    // argument `i` in `self.response_of(i)`. rust-analyzer resolves `i`
    // to `total`'s own PARAMETER declaration, which sits on the SAME
    // LINE as `total`'s function signature. A parameter is not itself an
    // indexed graph node, so an imprecise position match must not fall
    // back onto `total`'s own declaration and fabricate a `total ->
    // total` self-call edge. This assertion is the one the original PR's
    // test was missing — the false edge could be present with the test
    // above still green.
    let self_edges = calls_edges_from(&store, "src/lib.rs::TaskSet::total");
    assert!(
        self_edges.is_empty(),
        "the LSP pass must not fabricate a Calls edge from `total` to \
         itself or to anything but `response_of`: {self_edges:?}"
    );
}

/// Writes a minimal but real cargo package under a fresh tempdir: a struct
/// with a value-receiver method call the static resolver cannot bind
/// (`src/resolver/calls.rs` ~line 245). rust-analyzer needs the Cargo.toml
/// to load the workspace and answer `textDocument/definition` at all.
///
/// Returns the tempdir GUARD alongside the fixture's crate root — the guard
/// must be kept alive by the caller for as long as the path is used; see the
/// call site's comment on why.
fn write_receiver_call_fixture() -> (TestTempDir, PathBuf) {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("lsp_resolve_receiver_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);
    let fixture = tmp_root.join("fixture");
    fs::create_dir_all(&fixture).expect("mkdir fixture");

    fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname = \"lsp_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::create_dir_all(fixture.join("src")).expect("mkdir src");
    fs::write(
        fixture.join("src/lib.rs"),
        "pub struct TaskSet;\n\
         \n\
         impl TaskSet {\n\
         \x20\x20\x20\x20pub fn response_of(&self, i: i32) -> i32 { i }\n\
         \n\
         \x20\x20\x20\x20pub fn total(&self, i: i32) -> i32 {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20self.response_of(i)\n\
         \x20\x20\x20\x20}\n\
         }\n",
    )
    .expect("write lib.rs");
    (tmp_root, fixture)
}

/// Every caller `id` with a `Calls_Method_Method` edge into `response_of`.
fn callers_of_response_of(store: &GraphStore) -> Vec<String> {
    store
        .execute_query(&format!(
            "MATCH (a)-[:Calls_Method_Method]->(b:Method) \
             WHERE b.qualified_name = '{RESPONSE_OF_QN}' RETURN a.id"
        ))
        .expect("query callers")
        .rows
        .into_iter()
        .map(|row| row[0].clone())
        .collect()
}

/// Every `Calls_Method_Method` edge FROM `caller_id` other than to
/// `response_of` — i.e. any edge that would be a fabricated target,
/// including a self-edge.
fn calls_edges_from(store: &GraphStore, caller_id: &str) -> Vec<String> {
    store
        .execute_query(&format!(
            "MATCH (a:Method)-[:Calls_Method_Method]->(b:Method) \
             WHERE a.id = '{caller_id}' AND b.qualified_name <> '{RESPONSE_OF_QN}' \
             RETURN b.id"
        ))
        .expect("query edges from caller")
        .rows
        .into_iter()
        .map(|row| row[0].clone())
        .collect()
}
