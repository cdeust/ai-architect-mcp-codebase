// stage3b_resolution_arithmetic — regression tests for issue #28:
// "resolution_rate is arithmetically unsound: can exceed 1.0, collapses to
// ~0 on re-run, and counts failed inserts as successes."
//
// Three independent defects, three test groups:
//   1. `resolved + unresolved == total_refs` must hold exactly across every
//      reference kind (calls, uses, extends, macros) — this bounds
//      `resolution_rate` to [0.0, 1.0] by construction.
//   2. Re-running `resolve_graph` on an already-resolved graph must not
//      collapse the reported rate toward 0 (EdgeBuffer must count an
//      already-persisted edge as resolved, not as a no-op).
//   3. `resolve_extends` must only count successes, and must dedup like
//      every other edge kind.
//
// source: github.com/cdeust/ai-architect-mcp-codebase issue #28.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver::{self, ResolutionResult};
use std::fs;

/// Indexes `files` into a fresh graph and runs `resolve_graph` once.
/// Returns the result plus the open store (so callers can re-resolve or
/// inspect edge counts) and the tmp root (for cleanup).
fn index_and_open(
    tag: &str,
    files: &[(&str, &str)],
) -> (ResolutionResult, GraphStore, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("issue28_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let src = root.join("src");
    for (rel, body) in files {
        let p = src.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create fixture dir");
        }
        fs::write(&p, body).expect("write fixture file");
    }
    let graph_dir = root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let res = resolver::resolve_graph(&store).expect("resolve_graph");
    (res, store, root)
}

fn resolved_sum(r: &ResolutionResult) -> u64 {
    r.imports_resolved + r.calls_resolved + r.impls_resolved + r.extends_resolved + r.uses_resolved
}

fn assert_accounting_invariant(r: &ResolutionResult, label: &str) {
    assert_eq!(
        resolved_sum(r),
        r.total_edges,
        "{label}: sum of per-kind resolved counts must equal total_edges"
    );
    assert_eq!(
        r.total_edges + r.unresolved.len() as u64,
        r.total_refs,
        "{label}: resolved ({}) + unresolved ({}) must equal total_refs ({}); \
         a mismatch means resolution_rate can read outside [0.0, 1.0]",
        r.total_edges,
        r.unresolved.len(),
        r.total_refs
    );
    if r.total_refs > 0 {
        let rate = r.total_edges as f64 / r.total_refs as f64;
        assert!(
            (0.0..=1.0).contains(&rate),
            "{label}: resolution_rate {rate} must be in [0.0, 1.0]"
        );
    }
}

// ---------------------------------------------------------------------------
// (1) resolved + unresolved == total_refs, across calls + macros + uses.
// ---------------------------------------------------------------------------

const FIXTURE_CALLS_MACROS_USES: &str = r#"
pub struct Foo {
    pub x: i32,
}

pub struct Bar {
    pub y: i32,
}

// Uses phase: HashMap<Foo, Bar> extracts TWO type identifiers (Foo, Bar) —
// the denominator must count 2 refs here, not 1 (issue #28 defect #1b).
pub struct Holder {
    pub items: std::collections::HashMap<Foo, Bar>,
}

fn greet() {
    // Macro call — resolved exclusively by resolver_layers::run_macro_expansion.
    // issue #28 defect #1a: this used to be double-counted (once as an
    // unresolved plain-call attempt in resolve_calls, once as an
    // uncounted-denominator macro expansion).
    println!("hello");
    helper();
}

fn helper() -> i32 {
    42
}
"#;

#[test]
fn test_resolution_accounting_invariant_calls_macros_uses() {
    let (res, _store, root) = index_and_open(
        "calls_macros_uses",
        &[("lib.rs", FIXTURE_CALLS_MACROS_USES)],
    );
    eprintln!(
        "calls_macros_uses: calls={} uses={} total_edges={} total_refs={} unresolved={}",
        res.calls_resolved,
        res.uses_resolved,
        res.total_edges,
        res.total_refs,
        res.unresolved.len()
    );
    // Sanity: the fixture actually exercises calls, macro expansion, and uses.
    assert!(
        res.calls_resolved > 0,
        "expected the helper() call to resolve"
    );
    assert!(
        res.uses_resolved >= 1,
        "expected at least one Uses_Field_Struct edge from HashMap<Foo, Bar>"
    );

    assert_accounting_invariant(&res, "calls_macros_uses");
    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// (1b) resolved + unresolved == total_refs for Extends (Java: extends + implements).
// ---------------------------------------------------------------------------

const FIXTURE_JAVA_EXTENDS: &str = r#"
interface Greeter {
    String greet();
}

class Animal {
    void breathe() {}
}

class Dog extends Animal implements Greeter {
    public String greet() {
        return "woof";
    }
}
"#;

#[test]
fn test_resolution_accounting_invariant_extends() {
    let (res, _store, root) = index_and_open("extends", &[("Demo.java", FIXTURE_JAVA_EXTENDS)]);
    eprintln!(
        "extends: impls={} extends={} total_edges={} total_refs={} unresolved={}",
        res.impls_resolved,
        res.extends_resolved,
        res.total_edges,
        res.total_refs,
        res.unresolved.len()
    );
    assert!(
        res.extends_resolved > 0,
        "expected Dog -> Animal to resolve"
    );
    assert_accounting_invariant(&res, "extends");
    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// (2) Idempotence: re-running resolve_graph must not collapse the rate.
// ---------------------------------------------------------------------------

#[test]
fn test_resolution_idempotent_second_run_matches_first() {
    let (res1, store, root) = index_and_open(
        "idempotent",
        &[
            ("lib.rs", FIXTURE_CALLS_MACROS_USES),
            ("Demo.java", FIXTURE_JAVA_EXTENDS),
        ],
    );
    assert_accounting_invariant(&res1, "idempotent-run1");
    assert!(
        res1.total_refs > 0,
        "fixture must produce at least one reference"
    );
    let rate1 = res1.total_edges as f64 / res1.total_refs as f64;

    let edge_count_before = store.edge_count().expect("edge_count before");
    let res2 = resolver::resolve_graph(&store).expect("second resolve_graph");
    let edge_count_after = store.edge_count().expect("edge_count after");

    eprintln!(
        "idempotent: run1 total_edges={} total_refs={} rate={rate1:.4}; \
         run2 total_edges={} total_refs={}",
        res1.total_edges, res1.total_refs, res2.total_edges, res2.total_refs
    );

    assert_accounting_invariant(&res2, "idempotent-run2");
    assert_eq!(
        edge_count_before, edge_count_after,
        "re-running resolve_graph must not write duplicate physical edges"
    );

    // The core regression: before the fix, every buf.add() on a re-run
    // returned false (already in `seen`), so `resolved` counters stayed at
    // 0 while `total_refs` was unchanged — the rate collapsed to ~0 with no
    // real regression. Assert it stays within a tight epsilon of run 1.
    let rate2 = res2.total_edges as f64 / res2.total_refs as f64;
    assert!(
        (rate1 - rate2).abs() < 1e-9,
        "resolution_rate must be idempotent across re-runs with no source \
         changes: run1={rate1}, run2={rate2}"
    );
    assert!(
        res2.total_edges > 0,
        "second run's resolved count collapsed to 0 (issue #28 regression)"
    );

    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// (3) resolve_extends: only successful resolutions count, duplicates dedup.
// ---------------------------------------------------------------------------

// Python: `class Dog(Animal, Animal)` — a duplicate base name in one CSV.
// Both list entries are distinct references (each is a real `bases` token
// the parser extracted) that resolve to the identical target, so both must
// count as `resolved` (invariant), but only one physical edge may exist.
const FIXTURE_PY_DUPLICATE_BASE: &str = r#"
class Animal:
    def breathe(self):
        pass

class Dog(Animal, Animal):
    def bark(self):
        pass
"#;

#[test]
fn test_resolve_extends_dedups_duplicate_base_but_counts_both_resolved() {
    let (res, store, root) =
        index_and_open("extends_dup", &[("demo.py", FIXTURE_PY_DUPLICATE_BASE)]);
    eprintln!(
        "extends_dup: extends_resolved={} total_refs={} unresolved={}",
        res.extends_resolved,
        res.total_refs,
        res.unresolved.len()
    );
    assert_accounting_invariant(&res, "extends_dup");

    // Both "Animal" tokens in `Dog(Animal, Animal)` resolved successfully.
    assert_eq!(
        res.extends_resolved, 2,
        "both duplicate base-name references must count as resolved, got {}",
        res.extends_resolved
    );

    // But only one physical Extends_Struct_Struct edge exists in the store —
    // the dedup every other edge kind gets, which resolve_extends bypassed
    // before this fix (issue #28 defect #3).
    let qr = store
        .execute_query(
            "MATCH (a:Struct)-[r:Extends_Struct_Struct]->(b:Struct) \
             WHERE a.name = 'Dog' AND b.name = 'Animal' RETURN a.name, r.confidence",
        )
        .expect("query Extends_Struct_Struct");
    assert_eq!(
        qr.rows.len(),
        1,
        "duplicate base name must collapse to exactly one physical edge, got {}",
        qr.rows.len()
    );

    let _ = fs::remove_dir_all(&root);
}
