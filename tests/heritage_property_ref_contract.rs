// heritage_property_ref_contract — issue #216 step 3: make the walker/resolver
// contract EXPLICIT and LOUD, instead of implicit and silent.
//
// The defect class #212/#215 found (TypeScript) and #216 found again (Swift,
// Kotlin, ObjC, C++) is always the same shape: a walker emits an `Extends`/
// `Implements` REF for a heritage clause, but `resolver::extends`/
// `resolver::implements` only ever read the `bases`/`implements` CSV
// PROPERTY on the node — and `indexer::persist::edges::resolve_edge_table`
// silently drops raw `Extends`/`Implements` refs at ingestion (`"Extends" =>
// None`, `_ => None`), by design, because resolution is deferred to those two
// property-reading passes. A walker that emits the ref without also setting
// the property produces a heritage clause that parses correctly and resolves
// to NOTHING, with no error anywhere.
//
// This test makes that contract a build/test-time assertion, not a runtime
// hope: for every language, on a minimal fixture with one heritage clause,
// EVERY `Extends` ref's source node must carry a non-empty `bases` property,
// and every `Implements` ref's source node must carry a non-empty
// `implements` property. A future language that half-implements heritage
// extraction (refs only, no property) fails THIS test immediately, instead of
// silently producing zero graph edges that only a hand-run query would catch.
//
// Chosen over "have resolve_edge_table warn for a ref kind no resolver
// claims" (the issue's other suggested option): `Extends`/`Implements` ARE
// claimed by the resolver (resolve_extends/resolve_implements read their
// property), just not via the ref-ingestion path — a per-ref-kind warning at
// that call site cannot distinguish "this kind is deferred to the resolver,
// by design" from "this walker forgot to set the property the resolver
// reads." Asserting the actual node/ref pairing, per language, at the
// walker's own output, is the precise, loud, and false-positive-free check.

use ai_architect_mcp::parser::{parse_file, ExtractedNode, ExtractedRef, Language};

/// Asserts the walker/resolver contract holds for one language's output: every
/// `Extends` ref's source node carries a non-empty `bases` property, and every
/// `Implements` ref's source node carries a non-empty `implements` property.
/// precondition: `nodes`/`refs` are one language's `parse_file` output on a
/// fixture containing at least one heritage clause (callers assert that
/// separately, so a vacuously-empty fixture cannot pass by having no refs to
/// check — see the `_has_heritage_ref` assertion in each per-language test).
fn assert_heritage_contract(lang: &str, nodes: &[ExtractedNode], refs: &[ExtractedRef]) {
    for (ref_kind, property_key) in [("Extends", "bases"), ("Implements", "implements")] {
        for r in refs.iter().filter(|r| r.kind == ref_kind) {
            let node = nodes
                .iter()
                .find(|n| n.qualified_name == r.from_qualified_name)
                .unwrap_or_else(|| {
                    panic!(
                        "{lang}: {ref_kind} ref from {} names no extracted node",
                        r.from_qualified_name
                    )
                });
            let has_property = node
                .properties
                .iter()
                .any(|(k, v)| k == property_key && !v.is_empty());
            assert!(
                has_property,
                "{lang}: {} emits an {ref_kind} ref but has no non-empty `{property_key}` \
                 CSV property — resolver::extends/implements read that property, not the \
                 ref, and indexer::persist::edges::resolve_edge_table silently drops raw \
                 {ref_kind} refs at ingestion (issue #212/#216 defect class); node \
                 properties: {:?}",
                node.qualified_name, node.properties
            );
        }
    }
}

fn parse(lang: Language, path: &str, source: &str) -> (Vec<ExtractedNode>, Vec<ExtractedRef>) {
    let r = parse_file(source, path, lang).expect("parse must not hard-fail");
    assert_eq!(r.parse_errors, 0, "{lang:?}: fixture must parse clean");
    (r.nodes, r.refs)
}

#[test]
fn python_class_extends_satisfies_contract() {
    let (nodes, refs) = parse(
        Language::Python,
        "demo.py",
        "class Animal:\n    pass\n\n\nclass Dog(Animal):\n    pass\n",
    );
    assert!(
        refs.iter().any(|r| r.kind == "Extends"),
        "fixture must contain an Extends ref to exercise the contract"
    );
    assert_heritage_contract("python", &nodes, &refs);
}

#[test]
fn java_extends_and_implements_satisfy_contract() {
    let (nodes, refs) = parse(
        Language::Java,
        "Demo.java",
        "interface Greeter { String greet(); }\n\
         class Animal { void breathe() {} }\n\
         class Dog extends Animal implements Greeter { public String greet() { return \"woof\"; } }\n",
    );
    assert!(refs.iter().any(|r| r.kind == "Extends"));
    assert!(refs.iter().any(|r| r.kind == "Implements"));
    assert_heritage_contract("java", &nodes, &refs);
}

#[test]
fn typescript_extends_and_implements_satisfy_contract() {
    let (nodes, refs) = parse(
        Language::TypeScript,
        "demo.ts",
        "export interface Greeter { greet(): string; }\n\
         export class Animal { breathe(): void {} }\n\
         export class Dog extends Animal implements Greeter { greet(): string { return \"woof\"; } }\n",
    );
    assert!(refs.iter().any(|r| r.kind == "Extends"));
    assert!(refs.iter().any(|r| r.kind == "Implements"));
    assert_heritage_contract("typescript", &nodes, &refs);
}

#[test]
fn kotlin_extends_satisfies_contract() {
    let (nodes, refs) = parse(
        Language::Kotlin,
        "Demo.kt",
        "open class Animal {\n    fun breathe() {}\n}\n\nclass Dog : Animal() {\n    fun bark() {}\n}\n",
    );
    assert!(
        refs.iter().any(|r| r.kind == "Extends"),
        "fixture must contain an Extends ref to exercise the contract"
    );
    assert_heritage_contract("kotlin", &nodes, &refs);
}

#[test]
fn swift_extends_satisfies_contract() {
    let (nodes, refs) = parse(
        Language::Swift,
        "demo.swift",
        "class Animal {\n    func breathe() {}\n}\n\nclass Dog: Animal {\n    func bark() {}\n}\n",
    );
    assert!(
        refs.iter().any(|r| r.kind == "Extends"),
        "fixture must contain an Extends ref to exercise the contract"
    );
    assert_heritage_contract("swift", &nodes, &refs);
}

#[test]
fn objc_extends_satisfies_contract() {
    let (nodes, refs) = parse(
        Language::ObjC,
        "demo.m",
        "@interface Animal\n- (void)breathe;\n@end\n\n@interface Dog : Animal\n- (void)bark;\n@end\n",
    );
    assert!(
        refs.iter().any(|r| r.kind == "Extends"),
        "fixture must contain an Extends ref to exercise the contract"
    );
    assert_heritage_contract("objc", &nodes, &refs);
}

#[test]
fn cpp_extends_satisfies_contract() {
    let (nodes, refs) = parse(
        Language::Cpp,
        "demo.cpp",
        "class Animal {\npublic:\n    void breathe() {}\n};\n\nclass Dog : public Animal {\npublic:\n    void bark() {}\n};\n",
    );
    assert!(
        refs.iter().any(|r| r.kind == "Extends"),
        "fixture must contain an Extends ref to exercise the contract"
    );
    assert_heritage_contract("cpp", &nodes, &refs);
}
