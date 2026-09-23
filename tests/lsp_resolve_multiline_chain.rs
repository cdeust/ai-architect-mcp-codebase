// lsp_resolve_multiline_chain — end-to-end regression for issue #317.
//
// dy-wcet's `tests/properties.rs::generate` builds a task with a chain split
// across lines, each setter fed a local of the same name:
//
//     Task::new(wcet, period)
//         .deadline(deadline)
//
// The 2026-09-08 graph showed `generate -> Task::deadline`, but only because
// the #87 speculative scan emitted the ARGUMENT `deadline` as a call site and
// it matched the method by name. #294/#295 rightly removed that site, and the
// edge went with it, which exposed the real gap: the method call itself was
// never resolved, statically or by LSP. `lsp_position` kept the chain's first
// line and aimed rust-analyzer past its end. This test drives the real LSP
// pass on that shape. `#[ignore]`d when `rust-analyzer` is not on PATH, the
// reason is in the ignore string, per CONTRIBUTING.md's testing policy.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::lsp_client::is_command_available;
use ai_architect_mcp::{indexer, lsp_resolver, resolver};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

mod common;
use common::{TempDirExt, TestTempDir};

const DEADLINE_QN: &str = "src/lib.rs::Task::deadline";

#[test]
#[ignore = "requires rust-analyzer on PATH; run with `cargo test -- --ignored` \
            once `rustup component add rust-analyzer` or an equivalent install \
            has put it there"]
fn lsp_resolve_binds_a_method_on_a_later_line_of_a_chain() {
    if !is_command_available("rust-analyzer") {
        eprintln!("skipping: rust-analyzer not on PATH");
        return;
    }

    // `_tmp_root` must outlive the assertions: `TestTempDir::drop` deletes
    // its directory on a passing test.
    let (_tmp_root, fixture) = write_multiline_chain_fixture();
    let graph_dir = fixture.parent().expect("fixture parent").join("graph");
    indexer::index_codebase(&fixture, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open store");
    resolver::resolve_graph(&store).expect("static resolve");

    assert!(
        callers_of_deadline(&store).is_empty(),
        "the static resolver must NOT already bind this chain \
         (if it does, this fixture no longer exercises the LSP path)"
    );
    // #294/#295 must hold: the argument `deadline` is a parameter, a value,
    // and never a call site of its own.
    assert!(
        bare_call_sites_named(&store, "deadline").is_empty(),
        "the parameter `deadline` was emitted as a call site"
    );

    lsp_resolver::resolve_with_lsp(&store, &fixture, "rust", None, Duration::from_secs(30))
        .expect("lsp_resolve");

    assert_eq!(
        callers_of_deadline(&store),
        vec!["src/lib.rs::generate".to_string()],
        "`generate` calls `Task::deadline` on the chain's second line"
    );
}

/// A minimal cargo package whose free function `generate` calls
/// `Task::deadline` on a call result, on the line after the chain begins,
/// passing a parameter that shares the method's name.
fn write_multiline_chain_fixture() -> (TestTempDir, PathBuf) {
    let tmp_root = tempfile::Builder::new()
        .prefix("lsp_resolve_multiline_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);
    let fixture = tmp_root.join("fixture");
    fs::create_dir_all(fixture.join("src")).expect("mkdir src");
    fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname = \"lsp_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(
        fixture.join("src/lib.rs"),
        "pub struct Task {\n\
         \x20\x20\x20\x20pub d: u64,\n\
         }\n\
         \n\
         impl Task {\n\
         \x20\x20\x20\x20pub fn new(c: u64) -> Task { Task { d: c } }\n\
         \x20\x20\x20\x20pub fn deadline(self, d: u64) -> Task { Task { d } }\n\
         }\n\
         \n\
         pub fn generate(deadline: u64) -> Task {\n\
         \x20\x20\x20\x20Task::new(1)\n\
         \x20\x20\x20\x20\x20\x20\x20\x20.deadline(deadline)\n\
         }\n",
    )
    .expect("write lib.rs");
    (tmp_root, fixture)
}

/// Every caller `id` with a `Calls_Function_Method` edge into `deadline`.
fn callers_of_deadline(store: &GraphStore) -> Vec<String> {
    store
        .execute_query(&format!(
            "MATCH (a:Function)-[:Calls_Function_Method]->(b:Method) \
             WHERE b.qualified_name = '{DEADLINE_QN}' RETURN a.id"
        ))
        .expect("query callers")
        .rows
        .into_iter()
        .map(|row| row[0].clone())
        .collect()
}

/// Every `CallSite` whose callee is exactly the bare `name`.
fn bare_call_sites_named(store: &GraphStore, name: &str) -> Vec<String> {
    store
        .execute_query(&format!(
            "MATCH (c:CallSite) WHERE c.callee_name = '{name}' RETURN c.id"
        ))
        .expect("query call sites")
        .rows
        .into_iter()
        .map(|row| row[0].clone())
        .collect()
}
