// lsp_resolve_outside_targets — end-to-end regression for issue #284 (lot 5)
// against REAL rust-analyzer.
//
// `outside_targets_tests.rs` (unit-level, `src/lsp_resolver/`) already proves
// the skip against a fake server's request log. This file proves the same
// claim end-to-end: a real crate, a real `cargo metadata` target map, and a
// real rust-analyzer process, driven through the same `lsp_resolver::
// resolve_with_lsp` entry point `lsp_resolve`/`analyze_codebase` use.
// `#[ignore]`d when `rust-analyzer` is not on PATH — the reason is in the
// ignore string, per CONTRIBUTING.md's testing policy (mirrors
// `tests/lsp_resolve_receiver_calls.rs`).

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::lsp_client::is_command_available;
use ai_architect_mcp::{indexer, lsp_resolver, resolver};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

mod common;
use common::{TempDirExt, TestTempDir};

#[test]
#[ignore = "requires rust-analyzer on PATH; run with `cargo test -- --ignored` \
            once `rustup component add rust-analyzer` or an equivalent install \
            has put it there"]
fn lsp_resolve_attributes_the_kani_harness_site_outside_the_compiled_targets() {
    if !is_command_available("rust-analyzer") {
        eprintln!("skipping: rust-analyzer not on PATH");
        return;
    }
    if !is_command_available("cargo") {
        eprintln!("skipping: cargo not on PATH (cargo_targets::discover needs it)");
        return;
    }

    let (_tmp_root, fixture) = write_outside_target_fixture();
    let graph_dir = fixture
        .parent()
        .expect("fixture has a parent")
        .join("graph");
    indexer::index_codebase(&fixture, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open store");
    resolver::resolve_graph(&store).expect("static resolve");

    let result =
        lsp_resolver::resolve_with_lsp(&store, &fixture, "rust", None, Duration::from_secs(30))
            .expect("lsp_resolve");

    assert_eq!(
        result.outside_targets_count, 1,
        "exactly the kani/h.rs call site must be attributed outside the compiled \
         targets (resolved={}, failed={}, skipped={})",
        result.resolved_count, result.failed_count, result.skipped_count
    );
    assert_eq!(
        result.failed_count, 0,
        "the outside-target site must not also inflate failed_count \
         (resolved={}, outside={})",
        result.resolved_count, result.outside_targets_count
    );

    let reasons = callsite_unresolved_reasons_under(&store, "kani/h.rs::harness::call@");
    assert_eq!(
        reasons,
        vec!["outside_compiled_targets".to_string()],
        "the CallSite itself must carry the attribution"
    );
}

/// A real Cargo crate (one target, `src/lib.rs`) plus `kani/h.rs`, a file
/// the crate's own manifest never compiles — the exact shape of dy-wcet's
/// `kani/response_bounds.rs` gap this lot closes. `kani/h.rs`'s one call
/// site is a RECEIVER call (`w.value()`) from a free function, not a bare
/// name: a bare call resolves via the static resolver's `idx.by_name`
/// lookup regardless of which file it lives in (graph-only, LSP never
/// involved), which would leave nothing for the LSP pass to skip and defeat
/// this fixture's whole point. A receiver call from a free function (not a
/// `Method`, so lot 4's `self`/`Self` receiver binding does not apply
/// either) is exactly what stays unresolved until lot 6's local-binding
/// support lands — the same shape dy-wcet's Kani harnesses hit today.
///
/// Returns the tempdir GUARD alongside the fixture's crate root — the guard
/// must be kept alive by the caller for as long as the path is used (mirrors
/// `tests/lsp_resolve_receiver_calls.rs::write_receiver_call_fixture`).
fn write_outside_target_fixture() -> (TestTempDir, PathBuf) {
    let tmp_root = tempfile::Builder::new()
        .prefix("lsp_resolve_outside_targets_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);
    let fixture = tmp_root.join("fixture");
    fs::create_dir_all(fixture.join("src")).expect("mkdir src");
    fs::create_dir_all(fixture.join("kani")).expect("mkdir kani");

    fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname = \"outside_targets_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(
        fixture.join("src/lib.rs"),
        "pub struct Widget;\nimpl Widget {\n    pub fn value(&self) -> i32 { 1 }\n}\n",
    )
    .expect("write src/lib.rs");
    fs::write(
        fixture.join("kani/h.rs"),
        "fn harness() {\n    let w = Widget;\n    w.value();\n}\n",
    )
    .expect("write kani/h.rs");
    (tmp_root, fixture)
}

/// `unresolved_reason` of every `CallSite` whose id starts with `prefix` —
/// avoids hardcoding the parser's exact `call@line:col` suffix, which is an
/// implementation detail this test does not need to pin.
fn callsite_unresolved_reasons_under(store: &GraphStore, prefix: &str) -> Vec<String> {
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.id STARTS WITH '{prefix}' RETURN cs.unresolved_reason"
        ))
        .expect("query callsites")
        .rows
        .into_iter()
        .filter_map(|row| row.into_iter().next())
        .collect()
}
