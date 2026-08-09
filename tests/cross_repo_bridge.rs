// cross_repo_bridge — end-to-end test for the query-time cross-repo bridge.
//
// Builds TWO independent per-repo graphs (no super-graph, no shared index) and
// proves the bridge links them at query time:
//   - FORWARD:  a symbol unresolved in the consumer repo is found defined in
//               the provider repo (bridge::resolve_definition).
//   - REVERSE:  a symbol defined in the provider repo is found called from the
//               consumer repo (bridge::foreign_callers).
//   - HOMONYM:  a sibling that defines the same-named symbol locally is NOT
//               reported as a cross-repo caller (precision, not name-broadcast).
// source: cross-repo bridge spec (engineer memory 4197914).

use ai_architect_mcp::bridge::{self, SiblingGraphs};
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

// Provider repo (B): DEFINES process_payment. No local caller.
const PROVIDER_LIB: &str = r#"
pub fn process_payment(amount: i64) -> bool {
    amount > 0
}

pub fn refund(amount: i64) -> bool {
    amount > 0
}
"#;

// Consumer repo (A): CALLS process_payment but does NOT define it — a dangling
// external reference that the bridge resolves against the provider.
const CONSUMER_MAIN: &str = r#"
fn checkout() {
    let ok = process_payment(100);
    let _ = ok;
}

fn main() {
    checkout();
}
"#;

// Homonym repo (C): both DEFINES and CALLS process_payment locally. A call here
// is local, never a cross-repo edge to B.
const HOMONYM_MAIN: &str = r#"
fn process_payment(amount: i64) -> bool {
    amount > 0
}

fn main() {
    let _ = process_payment(5);
}
"#;

/// Indexes `source` as a single-file repo and resolves it; returns the graph dir.
fn build_graph(root: &Path, name: &str, file: &str, source: &str) -> PathBuf {
    let src = root.join(name).join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join(file), source).expect("write source");
    let graph_dir = root.join(name).join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    resolver::resolve_graph(&store).expect("resolve_graph");
    graph_dir
}

#[test]
fn cross_repo_forward_reverse_and_homonym() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let root = tempfile::Builder::new()
        .prefix("cross_repo_bridge_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&root);

    let provider = build_graph(&root, "provider", "lib.rs", PROVIDER_LIB);
    let consumer = build_graph(&root, "consumer", "main.rs", CONSUMER_MAIN);
    let homonym = build_graph(&root, "homonym", "main.rs", HOMONYM_MAIN);

    // --- FORWARD: from the consumer, resolve the dangling call into the provider.
    let args = json!({ "sibling_graphs": [provider.to_str().unwrap()] });
    let siblings = SiblingGraphs::from_arg(&args, &consumer);
    assert!(
        !siblings.is_empty(),
        "provider sibling should be registered"
    );

    let defs = bridge::resolve_definition(&siblings, "process_payment");
    assert!(
        defs.iter()
            .any(|d| d.qualified_name.contains("process_payment")
                && d.label == "Function"
                && d.repo == provider.display().to_string()),
        "forward resolution should find process_payment defined in the provider repo; got {:?}",
        defs.iter()
            .map(|d| (&d.repo, &d.label, &d.qualified_name))
            .collect::<Vec<_>>()
    );
    // A symbol the provider does NOT define must not resolve.
    assert!(
        bridge::resolve_definition(&siblings, "no_such_symbol").is_empty(),
        "unknown symbol must not resolve anywhere"
    );

    // --- REVERSE: from the provider, find the consumer's call as a foreign caller.
    let args = json!({ "sibling_graphs": [consumer.to_str().unwrap()] });
    let siblings = SiblingGraphs::from_arg(&args, &provider);
    let callers = bridge::foreign_callers(&siblings, "process_payment");
    assert!(
        callers
            .iter()
            .any(|c| c.repo == consumer.display().to_string() && c.caller_qn.contains("checkout")),
        "reverse resolution should find checkout() in the consumer as a foreign caller; got {:?}",
        callers
            .iter()
            .map(|c| (&c.repo, &c.caller_qn, &c.callee_name))
            .collect::<Vec<_>>()
    );

    // --- HOMONYM: a sibling that defines process_payment locally is skipped —
    // its call binds locally and is NOT a cross-repo edge to the provider.
    let args = json!({ "sibling_graphs": [homonym.to_str().unwrap()] });
    let siblings = SiblingGraphs::from_arg(&args, &provider);
    let callers = bridge::foreign_callers(&siblings, "process_payment");
    assert!(
        callers.is_empty(),
        "homonym repo defines process_payment locally → must not be a foreign caller; got {:?}",
        callers
            .iter()
            .map(|c| (&c.repo, &c.caller_qn))
            .collect::<Vec<_>>()
    );

    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Issue #31, defense 2 — the cross-repo bridge candidate input must exclude
// refs already classified external, regardless of whether the classifying
// provider's prefix list caught them (the two defenses must fail
// independently). Mirrors the filter applied in main.rs's do_resolve_graph
// before calling bridge::count_cross_repo_resolvable.
// ---------------------------------------------------------------------------

#[test]
fn kotlin_external_unresolveds_produce_empty_cross_repo_candidates() {
    let root = std::env::temp_dir().join(format!("cross_repo_kotlin_ext_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);

    // Single-repo Kotlin fixture: only JVM/Android ecosystem imports, no
    // real cross-repo siblings define any of these symbols.
    let consumer_src = "import androidx.lifecycle.ViewModel\nimport retrofit2.Retrofit\n\nclass MainViewModel : ViewModel() {\n    val retrofit: Retrofit? = null\n}\n";
    let consumer_src_dir = root.join("consumer").join("src");
    let consumer_graph_dir = root.join("consumer").join("graph");
    fs::create_dir_all(&consumer_src_dir).expect("create src");
    fs::write(consumer_src_dir.join("MainViewModel.kt"), consumer_src).expect("write fixture");
    indexer::index_codebase(&consumer_src_dir, &consumer_graph_dir).expect("index_codebase");
    let consumer_store = GraphStore::open_or_create(&consumer_graph_dir).expect("open graph");
    let result = resolver::resolve_graph(&consumer_store).expect("resolve_graph");

    // An unrelated sibling repo that defines unrelated symbols — proves an
    // empty result comes from the filter, not from an empty sibling set.
    let provider = build_graph(&root, "provider", "lib.rs", PROVIDER_LIB);

    let targets: Vec<String> = result
        .unresolved
        .iter()
        .filter(|u| u.reason != resolver::EXTERNAL_UNRESOLVED_REASON)
        .map(|u| u.target_text.clone())
        .collect();
    assert!(
        targets.is_empty(),
        "single-repo Kotlin fixture with only ecosystem imports must yield no cross-repo \
         candidate targets after filtering; got {:?}",
        targets
    );

    let args = json!({ "sibling_graphs": [provider.to_str().unwrap()] });
    let siblings = SiblingGraphs::from_arg(&args, &consumer_graph_dir);
    let (resolvable, sample) = bridge::count_cross_repo_resolvable(&siblings, &targets);
    assert_eq!(
        resolvable, 0,
        "cross-repo candidate count must be zero for an all-external unresolved set; got sample={:?}",
        sample
    );
    assert!(sample.is_empty());

    let _ = fs::remove_dir_all(&root);
}
