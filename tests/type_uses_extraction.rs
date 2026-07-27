//! Parser fidelity tests for issue #92 — return-type + type-construction
//! extraction (the `return_type` / `constructed_types` node properties the
//! resolver's Uses phase consumes). Split from `parser_fidelity.rs` to keep both
//! files under the 500-line cap (coding-standards §4.1). Pure in-memory parse
//! assertions — no DB, no indexer.
//!
//! Ground truth: tree-sitter-go 0.25.0 (`result` field, `composite_literal`),
//! tree-sitter-rust 0.23.3 (`return_type` field, `struct_expression`),
//! tree-sitter-typescript 0.23.2 (`return_type` field, `new_expression`). Each
//! assertion FAILS on the pre-#92 code (no property existed).

use ai_architect_mcp::parser::{self, Language, ParseResult};

fn parse(src: &str, path: &str, lang: Language) -> ParseResult {
    parser::parse_file(src, path, lang).expect("parse should not hard-fail")
}

/// The value of node `name`'s property `key`, or `None` if absent.
fn prop_of(r: &ParseResult, name: &str, key: &str) -> Option<String> {
    r.nodes.iter().find(|n| n.name == name).and_then(|n| {
        n.properties
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    })
}

#[test]
fn uses92_go_return_type_and_construction() {
    let src = "package p\n\
        type OrderConfig struct { Retries int }\n\
        func LoadConfig() OrderConfig { return OrderConfig{Retries: 3} }\n";
    let r = parse(src, "core.go", Language::Go);
    assert_eq!(
        prop_of(&r, "LoadConfig", "return_type").as_deref(),
        Some("OrderConfig"),
        "Go `result` field must be recorded as return_type"
    );
    assert_eq!(
        prop_of(&r, "LoadConfig", "constructed_types").as_deref(),
        Some("OrderConfig"),
        "Go `composite_literal` type must be recorded as a constructed type"
    );
}

#[test]
fn uses92_rust_return_type_and_construction() {
    let src = "pub struct OrderConfig { pub retries: u32 }\n\
        pub fn load_config() -> OrderConfig { OrderConfig { retries: 3 } }\n";
    let r = parse(src, "core.rs", Language::Rust);
    assert_eq!(
        prop_of(&r, "load_config", "return_type").as_deref(),
        Some("OrderConfig"),
        "Rust `return_type` (after `->`) must be recorded, arrow stripped"
    );
    assert_eq!(
        prop_of(&r, "load_config", "constructed_types").as_deref(),
        Some("OrderConfig"),
        "Rust `struct_expression` name must be recorded as a constructed type"
    );
}

#[test]
fn uses92_typescript_return_type_and_construction() {
    let src = "export class OrderConfig {}\n\
        export function loadConfig(): OrderConfig { return new OrderConfig(); }\n";
    let r = parse(src, "core.ts", Language::TypeScript);
    assert_eq!(
        prop_of(&r, "loadConfig", "return_type").as_deref(),
        Some("OrderConfig"),
        "TS `return_type` annotation must be recorded, leading `:` stripped"
    );
    assert_eq!(
        prop_of(&r, "loadConfig", "constructed_types").as_deref(),
        Some("OrderConfig"),
        "TS `new_expression` constructor must be recorded as a constructed type"
    );
}

#[test]
fn uses92_typescript_method_return_type() {
    let src = "export class Svc {\n  make(): Svc { return this; }\n}\n";
    let r = parse(src, "svc.ts", Language::TypeScript);
    assert_eq!(
        prop_of(&r, "make", "return_type").as_deref(),
        Some("Svc"),
        "TS method return_type must be recorded like a free function's"
    );
}

/// Negative (gating): Python did NOT adopt the #92 spec fields (its construction
/// is already a CallSite), so a Python function carries NEITHER property — this
/// is what keeps every non-adopting language's parity unchanged.
#[test]
fn uses92_python_records_no_return_or_construction() {
    let src = "class OrderConfig:\n    pass\n\ndef load_config():\n    return OrderConfig()\n";
    let r = parse(src, "core.py", Language::Python);
    assert!(
        prop_of(&r, "load_config", "return_type").is_none(),
        "Python must not record a return_type property (feature not adopted)"
    );
    assert!(
        prop_of(&r, "load_config", "constructed_types").is_none(),
        "Python must not record a constructed_types property (feature not adopted)"
    );
}
