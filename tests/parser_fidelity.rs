// Parser fidelity harness — pins the extraction behavior restored by the
// tree-sitter grammar-fidelity audit (findings F1-F12) so that (a) the
// module-split refactor is proven non-regressing in one `cargo test` run, and
// (b) future grammar-version drift fails loudly instead of silently producing
// plausible-but-empty node counts (the failure mode F3 was added to surface).
//
// Every assertion below FAILS on the pre-fix code and PASSES after the fixes.
// Ground truth: node-types.json for each grammar at its Cargo.lock-pinned tag
// (bundled in the audit artifact grammar-node-types-pinned.tar.gz).
//
// These are pure in-memory parse assertions — no DB, no indexer — so they run
// fast and isolate parser behavior from the rest of Stage 3.

use ai_architect_mcp::parser::{self, ExtractedNode, ExtractedRef, Language, ParseResult};

// ---- helpers ---------------------------------------------------------------

fn parse(src: &str, path: &str, lang: Language) -> ParseResult {
    parser::parse_file(src, path, lang).expect("parse should not hard-fail")
}

fn has_node(r: &ParseResult, label: &str, name_substr: &str) -> bool {
    r.nodes
        .iter()
        .any(|n: &ExtractedNode| n.label == label && n.name.contains(name_substr))
}

fn count_label(r: &ParseResult, label: &str) -> usize {
    r.nodes.iter().filter(|n| n.label == label).count()
}

fn has_ref(r: &ParseResult, kind: &str, to_substr: &str) -> bool {
    r.refs
        .iter()
        .any(|e: &ExtractedRef| e.kind == kind && e.to_qualified_name.contains(to_substr))
}

fn has_prop(r: &ParseResult, key: &str, val: &str) -> bool {
    r.nodes.iter().any(|n| {
        n.properties
            .iter()
            .any(|(k, v)| k.as_str() == key && v.as_str() == val)
    })
}

// ---- F3: parse errors are captured ----------------------------------------

#[test]
fn f3_clean_source_reports_zero_parse_errors() {
    let r = parse("fn main() { let x = 1; }\n", "a.rs", Language::Rust);
    assert_eq!(r.parse_errors, 0, "clean Rust must report 0 parse errors");
}

#[test]
fn f3_broken_source_reports_nonzero_parse_errors() {
    // Deliberately malformed — tree-sitter recovers into ERROR/MISSING nodes.
    let r = parse("fn main( { let = = ;;; ]]]\n", "bad.rs", Language::Rust);
    assert!(
        r.parse_errors > 0,
        "malformed Rust must report parse_errors > 0, got {}",
        r.parse_errors
    );
}

// ---- F1: Kotlin re-keyed to tree-sitter-kotlin-ng 1.1.0 --------------------

#[test]
fn f1_kotlin_imports_supertypes_calls_enums() {
    // Class/interface bodies are multi-line on purpose: tree-sitter-kotlin-ng
    // 1.1.0 requires a member separator (newline or `;`) before the closing
    // `}` of a `class_body`. A single-line body like `interface G { fun f() }`
    // is valid Kotlin but makes the grammar insert a MISSING `_class_member_semi`
    // (and, in a multi-declaration file, cascade into an ERROR that swallows the
    // following declarations). This is orthogonal to what F1 pins — the -ng node
    // re-keying — so we exercise the re-keying on the form the grammar parses
    // cleanly, which is also how real Kotlin is written.
    let src = r#"
package com.example
import kotlin.collections.List
interface Greeter {
    fun greet(): String
}
class Dog : Greeter {
    override fun greet(): String { return bark() }
}
enum class Color { RED, GREEN }
"#;
    let r = parse(src, "A.kt", Language::Kotlin);
    assert!(
        r.parse_errors == 0,
        "kotlin fixture should parse clean, got {}",
        r.parse_errors
    );
    assert!(
        has_node(&r, "Import", "List") || count_label(&r, "Import") >= 1,
        "F1: kotlin import dropped (import_header vs import)"
    );
    assert!(
        has_ref(&r, "Extends", "Greeter"),
        "F1: kotlin supertype Extends dropped (delegation_specifiers is a child, not a field)"
    );
    // enum entries RED/GREEN become Variant/Constant nodes reachable via enum_class_body
    assert!(
        count_label(&r, "Variant") + count_label(&r, "Constant") >= 2,
        "F1: kotlin enum entries dropped"
    );
    assert!(
        has_ref(&r, "Calls", "bark"),
        "F1: kotlin call callee dropped (callee field absent in -ng)"
    );
}

// ---- F2: ObjC re-keyed to tree-sitter-grammars/tree-sitter-objc 3.0.2 ------

#[test]
fn f2_objc_category_and_selector() {
    let src = r#"
#import <Foundation/Foundation.h>
@interface NSString (MyCategory)
- (void)doThingWith:(int)x andY:(int)y;
@end
"#;
    let r = parse(src, "A.m", Language::ObjC);
    // category is now a field on class_interface, not a distinct node kind
    assert!(
        has_prop(&r, "is_category", "true") || has_prop(&r, "category", "MyCategory"),
        "F2: objc category not detected"
    );
    // full keyword selector, not a degraded fallback fragment
    assert!(
        has_node(&r, "Method", "doThingWith"),
        "F2: objc selector degraded (keyword_declarator not walked)"
    );
}

#[test]
fn f2_objc_embedded_c_constructs() {
    // ObjC is a C superset — plain C decls in a .m must be extracted.
    let src = r#"
struct Point { int x, y; };
enum Dir { NORTH, SOUTH };
typedef int MyInt;
"#;
    let r = parse(src, "B.m", Language::ObjC);
    assert!(
        has_node(&r, "Struct", "Point"),
        "F2: embedded C struct dropped"
    );
    // multi-declarator `int x, y;` must yield two fields
    assert!(
        count_label(&r, "Field") >= 2,
        "F2: embedded C multi-declarator fields dropped"
    );
    assert!(has_node(&r, "Enum", "Dir"), "F2: embedded C enum dropped");
    assert!(
        has_prop(&r, "typedef", "true"),
        "F2: embedded C typedef dropped"
    );
}

// ---- F4: TSX dialect for .tsx / .jsx ---------------------------------------

#[test]
fn f4_tsx_parses_jsx_clean() {
    let src = "export const App = () => <div className=\"x\">{greet()}</div>;\n";
    let r = parse(src, "App.tsx", Language::TypeScript);
    assert_eq!(
        r.parse_errors, 0,
        "F4: JSX must parse clean under the TSX dialect, got {} errors",
        r.parse_errors
    );
    assert!(
        has_ref(&r, "Calls", "greet"),
        "F4: tsx JSX degraded call extraction"
    );
}

// ---- F5: C++ inheritance ---------------------------------------------------

#[test]
fn f5_cpp_base_class() {
    let src = "class Base {}; class Derived : public Base { void f(){} };\n";
    let r = parse(src, "a.cpp", Language::Cpp);
    assert!(
        has_ref(&r, "Extends", "Base"),
        "F5: cpp base class Extends dropped (bases field does not exist on class_specifier)"
    );
}

// ---- F8: Swift members + declaration_kind ----------------------------------

#[test]
fn f8_swift_members_and_enum_cases() {
    let src = r#"
struct S {
    init() {}
    subscript(i: Int) -> Int { return i }
}
enum E { case red, green, blue }
"#;
    let r = parse(src, "S.swift", Language::Swift);
    assert!(
        has_prop(&r, "member_kind", "init"),
        "F8: swift init not extracted"
    );
    assert!(
        has_prop(&r, "member_kind", "subscript"),
        "F8: swift subscript not extracted"
    );
    // multi-case `case red, green, blue` must yield three variants
    assert!(
        count_label(&r, "Variant") >= 3,
        "F8: swift multi-case enum entries dropped"
    );
}

// ---- F9: Go & C struct fields ----------------------------------------------

#[test]
fn f9_go_struct_fields() {
    let src = "package p\ntype Point struct {\n\tX, Y int\n}\n";
    let r = parse(src, "p.go", Language::Go);
    assert!(
        count_label(&r, "Field") >= 2,
        "F9: go struct multi-name fields dropped"
    );
}

#[test]
fn f9_c_struct_fields() {
    let src = "struct Point { int x, y; };\n";
    let r = parse(src, "p.c", Language::C);
    assert!(
        count_label(&r, "Field") >= 2,
        "F9: c struct multi-declarator fields dropped"
    );
}

// ---- F10: TS abstract/generator/var + Java enum constants ------------------

#[test]
fn f10_ts_abstract_and_generator() {
    let src = "abstract class Shape { abstract area(): number; }\nfunction* gen() { yield 1; }\n";
    let r = parse(src, "t.ts", Language::TypeScript);
    assert!(
        has_node(&r, "Struct", "Shape")
            || has_node(&r, "Trait", "Shape")
            || count_label(&r, "Struct") + count_label(&r, "Trait") >= 1,
        "F10: ts abstract class dropped"
    );
    assert!(
        has_node(&r, "Function", "gen"),
        "F10: ts generator function dropped"
    );
}

#[test]
fn f10_java_enum_constants() {
    let src = "enum Color { RED, GREEN, BLUE }\n";
    let r = parse(src, "C.java", Language::Java);
    assert!(
        count_label(&r, "Variant") >= 3,
        "F10: java enum constants dropped"
    );
}

// ---- F11: Rust statics / unions / macros / extern crate --------------------

#[test]
fn f11_rust_static_union_macro() {
    let src = r#"
static COUNT: u32 = 0;
union U { a: u32, b: f32 }
macro_rules! my_macro { () => {}; }
"#;
    let r = parse(src, "r.rs", Language::Rust);
    assert!(
        has_node(&r, "Constant", "COUNT"),
        "F11: rust static_item dropped"
    );
    assert!(has_node(&r, "Struct", "U"), "F11: rust union_item dropped");
    assert!(
        has_prop(&r, "is_macro", "true"),
        "F11: rust macro_definition dropped"
    );
}

// ---- issue #87 gap 3: Rust higher-order call (function passed as value) -----

/// True iff the parse produced a CallSite whose `callee_name` property equals
/// `callee` exactly.
fn has_callsite(r: &ParseResult, callee: &str) -> bool {
    callsite_line(r, callee).is_some()
}

/// The `start_line` of the CallSite whose `callee_name` equals `callee`, or
/// `None` if there is no such CallSite.
fn callsite_line(r: &ParseResult, callee: &str) -> Option<u64> {
    r.nodes
        .iter()
        .find(|n| {
            n.label == "CallSite"
                && n.properties
                    .iter()
                    .any(|(k, v)| k == "callee_name" && v == callee)
        })
        .map(|n| n.start_line)
}

#[test]
fn issue87_rust_higher_order_arg_is_captured_as_call_site() {
    // Derived from the #64 eval corpus (corpus/rust/worker.rs): `process_order`
    // is passed *by value* to `.map`, not called directly. Before the fix the
    // walker emitted CallSites only for the method chain
    // (`queue.iter().map(...).collect`), never for the function-value argument,
    // so `process_order` had no caller edge from `drain` and the graph lost the
    // "which functions call process_order?" row (rs-D2) to the Grep baseline.
    // Raw string so line numbers are unambiguous: `process_order` is on line 4.
    let src = r#"use crate::core::process_order;

pub fn drain(queue: Vec<i32>) -> Vec<i32> {
    queue.iter().map(process_order).collect()
}
"#;
    let r = parse(src, "worker.rs", Language::Rust);
    assert!(
        has_callsite(&r, "process_order"),
        "issue #87: a Rust function passed by value as a call argument \
         (`.map(process_order)`) must yield a CallSite with that callee so the \
         resolver can record the Calls edge; got callees {:?}",
        r_callees(&r)
    );
    // Pin the source span too (start_line) — the CallSite id and start_line are
    // derived from `row + 1`; asserting the exact line kills the off-by-one
    // mutant on that arithmetic (a wrong line breaks call-site identity /
    // navigation even when the callee name is right).
    assert_eq!(
        callsite_line(&r, "process_order"),
        Some(4),
        "the `process_order` argument call-site must report its real source line"
    );
}

/// All CallSite callee names in a parse result (diagnostic helper).
fn r_callees(r: &ParseResult) -> Vec<&str> {
    r.nodes
        .iter()
        .filter(|n| n.label == "CallSite")
        .filter_map(|n| {
            n.properties
                .iter()
                .find(|(k, _)| k == "callee_name")
                .map(|(_, v)| v.as_str())
        })
        .collect()
}

#[test]
fn issue87_rust_plain_value_arg_does_not_suppress_the_direct_call() {
    // Guard the sibling case: a direct call `process_order(order)` whose own
    // argument is a plain value must still emit the direct-call CallSite (the
    // function-value-argument capture is additive, never a replacement).
    let src = "fn f(order: i32) { process_order(order); }\n";
    let r = parse(src, "gateway.rs", Language::Rust);
    assert!(
        has_callsite(&r, "process_order"),
        "the direct call `process_order(order)` must still be captured"
    );
    // `order` is a local binding, not a function — it is emitted speculatively
    // and the resolver drops it, exactly as it drops `len` / `HashMap::new`.
    assert!(
        has_callsite(&r, "order"),
        "the value argument is emitted speculatively for the resolver to bind"
    );
}

// ---- Universal: every supported language yields SOME symbols on real code --

#[test]
fn universal_all_languages_extract_symbols() {
    // One representative source per language; each must yield >0 nodes AND
    // parse clean. This is the "reads any kind of file, same for all" guarantee.
    let cases: &[(&str, &str, Language)] = &[
        ("fn f() {}\n", "a.rs", Language::Rust),
        ("def f():\n    pass\n", "a.py", Language::Python),
        ("function f() {}\n", "a.ts", Language::TypeScript),
        ("class A { void f() {} }\n", "A.java", Language::Java),
        ("fun f() {}\n", "a.kt", Language::Kotlin),
        ("func f() {}\n", "a.swift", Language::Swift),
        ("void f() {}\n", "a.m", Language::ObjC),
        ("int f() { return 0; }\n", "a.c", Language::C),
        ("int f() { return 0; }\n", "a.cpp", Language::Cpp),
        ("package p\nfunc f() {}\n", "a.go", Language::Go),
    ];
    for (src, path, lang) in cases {
        let r = parse(src, path, *lang);
        assert_eq!(
            r.parse_errors, 0,
            "{path}: should parse clean, got {} errors",
            r.parse_errors
        );
        assert!(
            !r.nodes.is_empty(),
            "{path}: no nodes extracted for {lang:?}"
        );
    }
}
