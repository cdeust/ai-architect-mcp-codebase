// stage3b_integration — end-to-end test for the resolution pipeline.
//
// Creates a fixture Rust project with known cross-file references,
// indexes it (3a), resolves it (3b), and verifies resolution edges.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;

// ---------------------------------------------------------------------------
// Fixture source files — cross-file references for resolution testing
// ---------------------------------------------------------------------------

const FIXTURE_MAIN: &str = r#"
use crate::models::Config;
use crate::models::Processor;

fn main() {
    let cfg = Config::new("test".to_string());
    helper();
    init();
}

fn helper() -> i32 {
    42
}

fn init() -> bool {
    true
}
"#;

const FIXTURE_MODELS: &str = r#"
pub struct Config {
    pub name: String,
    pub max_retries: u32,
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Processor {
    fn process(&self, input: &str) -> String;
}

impl Config {
    pub fn new(name: String) -> Self {
        Config { name, max_retries: 3 }
    }
}

impl Processor for Config {
    fn process(&self, input: &str) -> String {
        input.to_string()
    }
}
"#;

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn test_resolution_pipeline() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3b_integration_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&tmp_root);

    // -- Set up fixture project --
    let fixture_dir = tmp_root.join("fixture");
    fs::create_dir_all(fixture_dir.join("src")).expect("create fixture/src");
    fs::write(fixture_dir.join("src/main.rs"), FIXTURE_MAIN).unwrap();
    fs::write(fixture_dir.join("src/models.rs"), FIXTURE_MODELS).unwrap();

    // -- Index the fixture (3a) --
    let graph_dir = tmp_root.join("graph");
    let idx_result = indexer::index_codebase(&fixture_dir.join("src"), &graph_dir)
        .expect("index_codebase should succeed");

    assert_eq!(idx_result.files_indexed, 2);
    assert!(idx_result.node_count > 0);
    assert!(idx_result.edge_count > 0);

    // -- Resolve the graph (3b) --
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let res = resolver::resolve_graph(&store).expect("resolve should succeed");

    // Should have resolved at least some imports
    assert!(
        res.imports_resolved > 0 || res.calls_resolved > 0 || res.uses_resolved > 0,
        "expected at least one resolution edge, got: imports={}, calls={}, uses={}",
        res.imports_resolved,
        res.calls_resolved,
        res.uses_resolved
    );
    assert!(res.total_edges > 0, "expected total_edges > 0");

    // -- Verify idempotency: resolve again --
    let edge_count_before = store.edge_count().expect("edge_count");
    let _res2 = resolver::resolve_graph(&store).expect("second resolve");
    let edge_count_after = store.edge_count().expect("edge_count after");
    // Idempotent: same edge count after second run
    assert_eq!(
        edge_count_before, edge_count_after,
        "resolution should be idempotent: before={}, after={}",
        edge_count_before, edge_count_after
    );

    // -- Verify call-site nodes exist --
    let cs_qr = store
        .execute_query("MATCH (cs:CallSite) RETURN count(cs)")
        .expect("count call sites");
    let cs_count: u64 = cs_qr.rows[0][0].parse().unwrap_or(0);
    assert!(
        cs_count > 0,
        "expected call site nodes to be extracted, got {cs_count}"
    );

    // -- Verify unresolved tracking works (may be empty if all resolve) --
    // The unresolved list tracks refs that failed resolution.
    // For this small fixture, most refs should resolve.
    eprintln!(
        "resolution stats: imports={}, calls={}, impls={}, extends={}, uses={}, unresolved={}",
        res.imports_resolved,
        res.calls_resolved,
        res.impls_resolved,
        res.extends_resolved,
        res.uses_resolved,
        res.unresolved.len()
    );

    // -- Cleanup --
    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_field_type_uses_resolution() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3b_uses_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture");
    fs::create_dir_all(fixture_dir.join("src")).unwrap();
    fs::write(
        fixture_dir.join("src/lib.rs"),
        r#"
pub struct Inner {
    pub value: i32,
}

pub struct Outer {
    pub child: Inner,
    pub name: String,
}
"#,
    )
    .unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&fixture_dir.join("src"), &graph_dir).expect("index");

    let store = GraphStore::open_or_create(&graph_dir).expect("open");
    let res = resolver::resolve_graph(&store).expect("resolve");

    // The `child: Inner` field should create a Uses_Field_Struct edge
    let qr = store
        .execute_query(
            "MATCH (f:Field)-[r:Uses_Field_Struct]->(s:Struct) \
         RETURN f.id, s.name",
        )
        .expect("query uses edges");

    assert!(
        !qr.rows.is_empty(),
        "expected Uses_Field_Struct edge for child: Inner, got none. \
         Resolution stats: uses={}",
        res.uses_resolved
    );

    let _ = fs::remove_dir_all(&tmp_root);
}

// Regression test for issue #1:
//   "analyze_codebase fails with 'unknown relationship type:
//    Uses_Field_TypeAlias' on real Rust codebases"
//
// Before the fix, indexing a struct whose field's type is a type alias
// caused the resolver to emit a Uses_Field_TypeAlias edge, which was
// never declared in the schema (graph_store.rs), in the indexer KNOWN
// whitelist (indexer.rs), or in the main.rs dispatch table. The whole
// analyze_codebase call aborted with status="error".
#[test]
fn test_field_type_alias_uses_resolution() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3b_uses_typealias_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture");
    fs::create_dir_all(fixture_dir.join("src")).unwrap();
    fs::write(
        fixture_dir.join("src/lib.rs"),
        r#"
pub type Payload = Vec<u8>;

pub struct Envelope {
    pub body: Payload,
    pub label: String,
}
"#,
    )
    .unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&fixture_dir.join("src"), &graph_dir).expect("index");

    let store = GraphStore::open_or_create(&graph_dir).expect("open");
    let res = resolver::resolve_graph(&store).expect("resolve");

    // The `body: Payload` field must produce a Uses_Field_TypeAlias edge.
    // If the schema / indexer / main.rs dispatch don't register that
    // edge table, resolve_graph returns an error before this point.
    let qr = store
        .execute_query(
            "MATCH (f:Field)-[r:Uses_Field_TypeAlias]->(t:TypeAlias) \
         RETURN f.id, t.name",
        )
        .expect("query Uses_Field_TypeAlias edges");

    assert!(
        !qr.rows.is_empty(),
        "expected Uses_Field_TypeAlias edge for body: Payload, got none. \
         Resolution stats: uses={}",
        res.uses_resolved
    );

    let _ = fs::remove_dir_all(&tmp_root);
}

// ---------------------------------------------------------------------------
// Implements resolution — declared facts, not method-name guesses.
// source: implements fix — verifies both the `#[derive(...)]` path (→ stdlib
// trait via the macro table) and the `impl Trait for Type` path (→ local
// Trait), neither of which the prior fuzzy stub produced reliably.
// ---------------------------------------------------------------------------

const FIXTURE_IMPLEMENTS: &str = r#"
pub trait Greet {
    fn greet(&self) -> String;
}

#[derive(Debug, Clone)]
pub struct Robot {
    pub id: i32,
}

impl Greet for Robot {
    fn greet(&self) -> String {
        format!("beep {}", self.id)
    }
}
"#;

#[test]
fn test_implements_resolution_declared() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3b_impl_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&tmp_root);
    let src = tmp_root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), FIXTURE_IMPLEMENTS).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open");
    let res = resolver::resolve_graph(&store).expect("resolve");

    // (B) impl Trait for Type: Robot implements the local Greet trait.
    let trait_edge = store
        .execute_query(
            "MATCH (s:Struct)-[:Implements_Struct_Trait]->(t:Trait) \
         WHERE s.name = 'Robot' RETURN t.name",
        )
        .expect("query Implements_Struct_Trait");
    assert!(
        trait_edge
            .rows
            .iter()
            .any(|r| r.first().map(|n| n == "Greet").unwrap_or(false)),
        "Robot must implement local trait Greet (impl-block path); impls={}",
        res.impls_resolved
    );

    // (A) #[derive(Debug)]: Robot implements std::fmt::Debug via the macro table.
    let std_edge = store
        .execute_query(
            "MATCH (s:Struct)-[:Implements_Struct_StdlibSymbol]->(d:StdlibSymbol) \
         WHERE s.name = 'Robot' RETURN d.id",
        )
        .expect("query Implements_Struct_StdlibSymbol");
    assert!(
        std_edge
            .rows
            .iter()
            .any(|r| r.first().map(|n| n == "std::fmt::Debug").unwrap_or(false)),
        "Robot must implement std::fmt::Debug via #[derive(Debug)]; got {:?}",
        std_edge.rows
    );

    // The prior fuzzy stub would have guessed Implements edges from method-name
    // coincidence; with declared resolution, greet() alone must NOT manufacture
    // a spurious trait edge to any trait lacking a real impl. Greet is the only
    // implemented trait, so exactly one local-trait edge from Robot.
    assert_eq!(
        trait_edge.rows.len(),
        1,
        "exactly one declared local-trait impl expected, got {:?}",
        trait_edge.rows
    );

    let _ = fs::remove_dir_all(&tmp_root);
}

// ---------------------------------------------------------------------------
// Java implements + extends — the parser now populates the bases/implements
// columns (previously emitted only as dropped refs), so the generic resolver
// passes produce real edges. source: implements fix (Java).
// ---------------------------------------------------------------------------

const FIXTURE_JAVA: &str = r#"
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
fn test_java_implements_and_extends_resolution() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3b_java_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&tmp_root);
    let src = tmp_root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("Demo.java"), FIXTURE_JAVA).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open");
    let res = resolver::resolve_graph(&store).expect("resolve");

    // Java `implements`: Dog -> Greeter (interface carries the Trait label).
    let impl_edge = store
        .execute_query(
            "MATCH (s:Struct)-[:Implements_Struct_Trait]->(t:Trait) \
         WHERE s.name = 'Dog' RETURN t.name",
        )
        .expect("query Implements_Struct_Trait");
    assert!(
        impl_edge
            .rows
            .iter()
            .any(|r| r.first().map(|n| n == "Greeter").unwrap_or(false)),
        "Dog must implement Greeter (Java implements); impls={}, got {:?}",
        res.impls_resolved,
        impl_edge.rows
    );

    // Java `extends`: Dog -> Animal.
    let ext_edge = store
        .execute_query(
            "MATCH (a:Struct)-[:Extends_Struct_Struct]->(b:Struct) \
         WHERE a.name = 'Dog' RETURN b.name",
        )
        .expect("query Extends_Struct_Struct");
    assert!(
        ext_edge
            .rows
            .iter()
            .any(|r| r.first().map(|n| n == "Animal").unwrap_or(false)),
        "Dog must extend Animal (Java extends); extends={}, got {:?}",
        res.extends_resolved,
        ext_edge.rows
    );

    let _ = fs::remove_dir_all(&tmp_root);
}

// ---------------------------------------------------------------------------
// TypeScript implements + extends — issue #212 root cause 2. The dedicated
// TS walker (parser::spec::walkers::typescript) only ever emitted
// `Extends`/`Implements` REFS for `class X extends Y implements Z` /
// `interface A extends B`; it never wrote the `bases`/`implements` CSV
// properties `resolver::extends::resolve_extends` /
// `resolver::implements::resolve_implements` actually read (`s.bases`,
// `s.implements`), and `indexer::persist::edges::resolve_edge_table` drops
// raw `Extends`/`Implements` refs (kind not in its match arms) rather than
// persisting them directly. So every TS `implements`/class-`extends`/
// interface-`extends` clause parsed correctly but produced zero graph
// edges. Fixed by having the TS walker populate the same CSV properties
// Java's `class_inheritance` convention does.
// ---------------------------------------------------------------------------

const FIXTURE_TYPESCRIPT: &str = r#"
export interface Greeter {
    greet(): string;
}

export interface Named extends Greeter {
    name: string;
}

export class Animal {
    breathe(): void {}
}

export class Dog extends Animal implements Named {
    name: string = "Rex";
    greet(): string {
        return "woof";
    }
}
"#;

#[test]
fn test_typescript_implements_and_extends_resolution() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3b_typescript_")
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&tmp_root);
    let src = tmp_root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("demo.ts"), FIXTURE_TYPESCRIPT).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open");
    let res = resolver::resolve_graph(&store).expect("resolve");

    // TS `implements`: Dog -> Named (interface carries the Trait label).
    let impl_edge = store
        .execute_query(
            "MATCH (s:Struct)-[:Implements_Struct_Trait]->(t:Trait) \
         WHERE s.name = 'Dog' RETURN t.name",
        )
        .expect("query Implements_Struct_Trait");
    assert!(
        impl_edge
            .rows
            .iter()
            .any(|r| r.first().map(|n| n == "Named").unwrap_or(false)),
        "Dog must implement Named (TS implements); impls={}, got {:?}",
        res.impls_resolved,
        impl_edge.rows
    );

    // TS class `extends`: Dog -> Animal.
    let ext_edge = store
        .execute_query(
            "MATCH (a:Struct)-[:Extends_Struct_Struct]->(b:Struct) \
         WHERE a.name = 'Dog' RETURN b.name",
        )
        .expect("query Extends_Struct_Struct");
    assert!(
        ext_edge
            .rows
            .iter()
            .any(|r| r.first().map(|n| n == "Animal").unwrap_or(false)),
        "Dog must extend Animal (TS extends); extends={}, got {:?}",
        res.extends_resolved,
        ext_edge.rows
    );

    // TS interface `extends`: Named -> Greeter (Trait carries `bases` too).
    let iface_ext_edge = store
        .execute_query(
            "MATCH (a:Trait)-[:Extends_Trait_Trait]->(b:Trait) \
         WHERE a.name = 'Named' RETURN b.name",
        )
        .expect("query Extends_Trait_Trait");
    assert!(
        iface_ext_edge
            .rows
            .iter()
            .any(|r| r.first().map(|n| n == "Greeter").unwrap_or(false)),
        "Named must extend Greeter (TS interface extends); got {:?}",
        iface_ext_edge.rows
    );

    let _ = fs::remove_dir_all(&tmp_root);
}
