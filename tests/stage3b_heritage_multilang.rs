// stage3b_heritage_multilang — issue #216: does the #212/#215 heritage-edge
// defect class (walker emits an `Extends`/`Implements` REF, but the resolver
// reads the `bases`/`implements` CSV PROPERTY the walker never set, and
// `indexer::persist::edges::resolve_edge_table` drops raw `Extends`/
// `Implements` refs — so a syntactically-correct heritage clause produces
// zero graph edges) also affect Python, Swift, ObjC, Kotlin, and C++?
//
// One minimal single-inheritance fixture per language, indexed + resolved
// through the real pipeline exactly like `test_typescript_implements_and_
// extends_resolution` / `test_java_implements_and_extends_resolution` in
// `stage3b_integration.rs`. Java and TypeScript are not repeated here (already
// covered by that file and already fixed); this file is additive for the five
// remaining languages the issue names.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;

/// Indexes+resolves a single-file fixture and returns the store, so each
/// per-language test can query its own inheritance edge.
fn index_and_resolve(prefix: &str, file_name: &str, source: &str) -> GraphStore {
    let tmp_root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&tmp_root);
    let src = tmp_root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(file_name), source).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open");
    resolver::resolve_graph(&store).expect("resolve");
    store
}

// ---------------------------------------------------------------------------
// Python — class_inheritance uses the shared default
// (LanguageConventions::class_inheritance, conventions.rs), which already
// sets the `bases` CSV property alongside the `Extends` ref. Expected: edge
// present already; this test is a regression pin, not a fix target.
// ---------------------------------------------------------------------------

const FIXTURE_PYTHON: &str = r#"
class Animal:
    def breathe(self):
        pass


class Dog(Animal):
    def bark(self):
        pass
"#;

#[test]
fn test_python_extends_resolution() {
    let store = index_and_resolve("stage3b_python_", "demo.py", FIXTURE_PYTHON);
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
        "Dog must extend Animal (Python extends); got {:?}",
        ext_edge.rows
    );
}

// ---------------------------------------------------------------------------
// Swift — SwiftConventions::class_inheritance (swift.rs) emits an `Extends`
// ref per `inheritance_specifier` but records NO `bases` property at all
// ("No `bases` property is recorded (Kotlin parity...)"). Same defect class
// as #212 root cause 2: resolver::extends::resolve_extends reads `s.bases`,
// which is always empty for Swift, so the ref is silently unresolvable.
// ---------------------------------------------------------------------------

const FIXTURE_SWIFT: &str = r#"
class Animal {
    func breathe() {}
}

class Dog: Animal {
    func bark() {}
}
"#;

#[test]
fn test_swift_extends_resolution() {
    let store = index_and_resolve("stage3b_swift_", "demo.swift", FIXTURE_SWIFT);
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
        "Dog must extend Animal (Swift extends); got {:?}",
        ext_edge.rows
    );
}

// ---------------------------------------------------------------------------
// Objective-C — the dedicated walker (walkers/objc/mod.rs::emit_class) emits
// an `Extends` ref per `superclass_field` but never sets a `bases` property
// on the `Struct` node. Same defect class.
// ---------------------------------------------------------------------------

const FIXTURE_OBJC: &str = r#"
@interface Animal
- (void)breathe;
@end

@interface Dog : Animal
- (void)bark;
@end
"#;

#[test]
fn test_objc_extends_resolution() {
    let store = index_and_resolve("stage3b_objc_", "demo.m", FIXTURE_OBJC);
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
        "Dog must extend Animal (ObjC superclass); got {:?}",
        ext_edge.rows
    );
}

// ---------------------------------------------------------------------------
// Kotlin — KotlinConventions::class_inheritance (kotlin_conventions.rs) emits
// an `Extends` ref per `delegation_specifiers` entry but records NO `bases`
// property ("No `bases` property is recorded (parity with the hand-written
// walker...)"). Same defect class.
// ---------------------------------------------------------------------------

const FIXTURE_KOTLIN: &str = r#"
open class Animal {
    fun breathe() {}
}

class Dog : Animal() {
    fun bark() {}
}
"#;

#[test]
fn test_kotlin_extends_resolution() {
    let store = index_and_resolve("stage3b_kotlin_", "Demo.kt", FIXTURE_KOTLIN);
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
        "Dog must extend Animal (Kotlin extends); got {:?}",
        ext_edge.rows
    );
}

// ---------------------------------------------------------------------------
// C++ — the dedicated walker (walkers/cpp.rs::emit_bases) emits an `Extends`
// ref per base_type_kinds child of a `base_class_clause` but never sets a
// `bases` property on the `Struct` node. Same defect class.
// ---------------------------------------------------------------------------

const FIXTURE_CPP: &str = r#"
class Animal {
public:
    void breathe() {}
};

class Dog : public Animal {
public:
    void bark() {}
};
"#;

#[test]
fn test_cpp_extends_resolution() {
    let store = index_and_resolve("stage3b_cpp_", "demo.cpp", FIXTURE_CPP);
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
        "Dog must extend Animal (C++ public inheritance); got {:?}",
        ext_edge.rows
    );
}
