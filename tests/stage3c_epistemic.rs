// stage3c_epistemic — end-to-end test for epistemic-boundary honesty in
// get_impact (Phase 1 "honest impact").
//
// Fixture: a trait with two implementors and a polymorphic caller. A trait is a
// dynamic-dispatch surface, so its statically-captured dependent set is a LOWER
// BOUND on true impact — get_impact must report `epistemic == LowerBound` with a
// non-empty reason. A concrete leaf function with no dependents must report
// `epistemic == Exact`.
// source: epistemic module contract; checkpoint Phase 1 #1.

use ai_architect_mcp::clustering;
use ai_architect_mcp::epistemic::Boundary;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;
mod common;
use common::TempDirExt;

// A trait (dynamic-dispatch surface), two declared implementors, and a function
// that invokes the trait method through a trait object.
const FIXTURE: &str = r#"
pub trait Greeter {
    fn greet(&self) -> String;
}

pub struct English;
impl Greeter for English {
    fn greet(&self) -> String { "hello".to_string() }
}

pub struct French;
impl Greeter for French {
    fn greet(&self) -> String { "bonjour".to_string() }
}

pub fn run(g: &dyn Greeter) -> String {
    g.greet()
}

pub fn standalone_leaf() -> i32 {
    7
}
"#;

#[test]
fn test_get_impact_reports_lower_bound_for_interface() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3c_epistemic_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture/src");
    fs::create_dir_all(&fixture_dir).expect("create fixture");
    fs::write(fixture_dir.join("lib.rs"), FIXTURE).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&fixture_dir, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    resolver::resolve_graph(&store).expect("resolve_graph");

    // Fetch the trait's stored qualified_name rather than guessing its on-disk
    // form (the path-stripped `lib.rs::Greeter` convention).
    let qr = store
        .execute_query("MATCH (t:Trait) RETURN t.qualified_name")
        .expect("query traits");
    assert!(!qr.rows.is_empty(), "fixture must produce a Trait node");
    let trait_qn = qr.rows[0][0].clone();

    let impact = clustering::get_impact(&store, &trait_qn).expect("get_impact(trait)");

    // The two declared `impl Greeter for _` blocks must surface as implementors.
    assert_eq!(
        impact.implementors.len(),
        2,
        "expected 2 implementors (English, French), got {}: {:?}",
        impact.implementors.len(),
        impact
            .implementors
            .iter()
            .map(|n| &n.qualified_name)
            .collect::<Vec<_>>()
    );

    // Core assertion: a trait is a dynamic-dispatch surface → lower bound.
    assert_eq!(
        impact.epistemic,
        Boundary::LowerBound,
        "trait target must be reported as a lower bound, got {:?}",
        impact.epistemic
    );
    assert!(
        impact
            .epistemic_reasons
            .iter()
            .any(|r| r.contains("dynamic-dispatch")),
        "epistemic_reasons must name dynamic dispatch, got {:?}",
        impact.epistemic_reasons
    );

    // Every implementor edge carries a confidence in (0.0, 1.0]; declared
    // impl-block resolution stores 0.95. source: resolver.rs:658.
    for node in &impact.implementors {
        assert!(
            node.confidence > 0.0 && node.confidence <= 1.0,
            "implementor {} has out-of-range confidence {}",
            node.qualified_name,
            node.confidence
        );
    }
}

#[test]
fn test_get_impact_reports_exact_for_concrete_leaf() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3c_epistemic_leaf_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture/src");
    fs::create_dir_all(&fixture_dir).expect("create fixture");
    fs::write(fixture_dir.join("lib.rs"), FIXTURE).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&fixture_dir, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    resolver::resolve_graph(&store).expect("resolve_graph");

    // `standalone_leaf` is a concrete Function with no callers and no
    // dynamic-dispatch surface — its (empty) dependent set is exhaustive.
    let qr = store
        .execute_query(
            "MATCH (f:Function) WHERE f.name = 'standalone_leaf' \
             RETURN f.qualified_name",
        )
        .expect("query leaf");
    assert!(!qr.rows.is_empty(), "fixture must produce standalone_leaf");
    let leaf_qn = qr.rows[0][0].clone();

    let impact = clustering::get_impact(&store, &leaf_qn).expect("get_impact(leaf)");

    assert_eq!(
        impact.epistemic,
        Boundary::Exact,
        "concrete leaf with no dependents must be exact, got {:?} (reasons: {:?})",
        impact.epistemic,
        impact.epistemic_reasons
    );
    assert!(
        impact.epistemic_reasons.is_empty(),
        "exact result must carry no reasons, got {:?}",
        impact.epistemic_reasons
    );
}
