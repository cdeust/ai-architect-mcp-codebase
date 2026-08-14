// Integration test: multi-language indexing with auto-detection.
//
// Indexes a fixture directory containing .rs, .py, and .ts files
// and verifies each language produces the expected nodes and edges.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::parser::{self, Language};
use std::path::Path;
mod common;
use common::TempDirExt;

#[test]
fn test_multilang_auto_index() {
    let fixture = Path::new("tests/fixtures/multilang");
    // issue #25 audit: process::id() is IDENTICAL for every #[test] in this
    // binary (all run as threads of one process) and can additionally repeat
    // across process invocations under PID reuse (observed: this test and
    // test_language_filter_rust_only both hit "duplicated primary key"
    // errors under a back-to-back soak — a leftover DB from a prior process
    // that got the same recycled PID). tempfile's random suffix depends on
    // neither the thread nor the OS PID.
    let tmp = tempfile::Builder::new()
        .prefix("multilang_test_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&tmp);

    let result =
        indexer::index_codebase_with_language(fixture, &tmp, &indexer::IndexOptions::default())
            .expect("index should succeed");

    assert_eq!(result.files_indexed, 3, "should index 3 files (rs, py, ts)");
    assert!(
        result.node_count > 15,
        "should have many nodes, got {}",
        result.node_count
    );
    assert!(
        result.edge_count > 5,
        "should have edges, got {}",
        result.edge_count
    );

    // Query the graph for language-specific symbols
    let store = GraphStore::open_or_create(&tmp).unwrap();

    // Rust: 3 functions (greet, add, fetch_data) + 1 struct (Config) + 2 fields
    let rust_fns = store
        .execute_query(
            "MATCH (f:Function) WHERE f.qualified_name STARTS WITH 'sample.rs' RETURN f.name",
        )
        .unwrap();
    assert!(
        rust_fns.rows.len() >= 3,
        "should find Rust functions, got {:?}",
        rust_fns.rows
    );

    let rust_structs = store
        .execute_query(
            "MATCH (s:Struct) WHERE s.qualified_name STARTS WITH 'sample.rs' RETURN s.name",
        )
        .unwrap();
    assert!(
        !rust_structs.rows.is_empty(),
        "should find Rust struct Config, got {:?}",
        rust_structs.rows
    );

    // Python: 2 functions (greet, add) + 1 struct/class (Config) + methods
    let py_fns = store
        .execute_query(
            "MATCH (f:Function) WHERE f.qualified_name STARTS WITH 'sample.py' RETURN f.name",
        )
        .unwrap();
    assert!(
        py_fns.rows.len() >= 2,
        "should find Python functions, got {:?}",
        py_fns.rows
    );

    let py_classes = store
        .execute_query(
            "MATCH (s:Struct) WHERE s.qualified_name STARTS WITH 'sample.py' RETURN s.name",
        )
        .unwrap();
    assert!(
        !py_classes.rows.is_empty(),
        "should find Python class Config (as Struct), got {:?}",
        py_classes.rows
    );

    // TypeScript: function greet + class Config + interface Serializable
    let ts_fns = store
        .execute_query(
            "MATCH (f:Function) WHERE f.qualified_name STARTS WITH 'sample.ts' RETURN f.name",
        )
        .unwrap();
    assert!(
        !ts_fns.rows.is_empty(),
        "should find TypeScript functions, got {:?}",
        ts_fns.rows
    );

    let ts_traits = store
        .execute_query(
            "MATCH (t:Trait) WHERE t.qualified_name STARTS WITH 'sample.ts' RETURN t.name",
        )
        .unwrap();
    assert!(
        !ts_traits.rows.is_empty(),
        "should find TypeScript interface (as Trait), got {:?}",
        ts_traits.rows
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_language_filter_rust_only() {
    let fixture = Path::new("tests/fixtures/multilang");
    // issue #25 audit: see test_multilang_auto_index, above, for why
    // process::id() is unsafe here.
    let tmp = tempfile::Builder::new()
        .prefix("multilang_rust_only_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = std::fs::remove_dir_all(&tmp);

    let result = indexer::index_codebase_with_language(
        fixture,
        &tmp,
        &indexer::IndexOptions {
            language_filter: Some(Language::Rust),
            dependency_scope: indexer::DependencyScope::None,
            exclude_dirs: indexer::ExcludeSet::default(),
        },
    )
    .expect("index should succeed");

    assert_eq!(result.files_indexed, 1, "should only index 1 .rs file");

    let store = GraphStore::open_or_create(&tmp).unwrap();
    let py_nodes = store
        .execute_query(
            "MATCH (f:Function) WHERE f.qualified_name STARTS WITH 'sample.py' RETURN f.name",
        )
        .unwrap();
    assert!(py_nodes.rows.is_empty(), "should NOT have Python nodes");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_python_parser_standalone() {
    let src = r#"
import os
from pathlib import Path

MAX_SIZE = 100

def greet(name: str) -> str:
    return f"Hello, {name}"

class Animal:
    def __init__(self, name):
        self.name = name

    def speak(self):
        pass

class Dog(Animal):
    def speak(self):
        return "Woof"
"#;
    let result =
        parser::parse_file(src, "test.py", Language::Python).expect("parse should succeed");

    let fn_count = result
        .nodes
        .iter()
        .filter(|n| n.label == "Function")
        .count();
    let class_count = result.nodes.iter().filter(|n| n.label == "Struct").count();
    let method_count = result.nodes.iter().filter(|n| n.label == "Method").count();
    let import_count = result.nodes.iter().filter(|n| n.label == "Import").count();
    let const_count = result
        .nodes
        .iter()
        .filter(|n| n.label == "Constant")
        .count();

    assert!(fn_count >= 1, "should find functions, got {fn_count}");
    assert!(
        class_count >= 2,
        "should find classes (Animal, Dog), got {class_count}"
    );
    assert!(method_count >= 3, "should find methods, got {method_count}");
    assert!(import_count >= 2, "should find imports, got {import_count}");
    assert!(
        const_count >= 1,
        "should find constant MAX_SIZE, got {const_count}"
    );

    // Extends edge
    let extends = result
        .refs
        .iter()
        .any(|r| r.kind == "Extends" && r.from_qualified_name.contains("Dog"));
    assert!(extends, "Dog should extend Animal");
}

#[test]
fn test_typescript_parser_standalone() {
    let src = r#"
import { Router } from 'express';

export const MAX_RETRIES = 3;

export function greet(name: string): string {
    return `Hello, ${name}`;
}

export class Animal {
    public name: string;

    speak(): string {
        return "";
    }
}

export class Dog extends Animal {
    speak(): string {
        return "Woof";
    }
}

export interface Serializable {
    serialize(): string;
}

export enum Color {
    Red = "RED",
    Green = "GREEN",
}

export type Alias = string | number;
"#;
    let result =
        parser::parse_file(src, "test.ts", Language::TypeScript).expect("parse should succeed");

    let fn_count = result
        .nodes
        .iter()
        .filter(|n| n.label == "Function")
        .count();
    let class_count = result.nodes.iter().filter(|n| n.label == "Struct").count();
    let trait_count = result.nodes.iter().filter(|n| n.label == "Trait").count();
    let enum_count = result.nodes.iter().filter(|n| n.label == "Enum").count();
    let type_alias_count = result
        .nodes
        .iter()
        .filter(|n| n.label == "TypeAlias")
        .count();
    let import_count = result.nodes.iter().filter(|n| n.label == "Import").count();
    let const_count = result
        .nodes
        .iter()
        .filter(|n| n.label == "Constant")
        .count();

    assert!(fn_count >= 1, "should find function greet, got {fn_count}");
    assert!(
        class_count >= 2,
        "should find classes (Animal, Dog), got {class_count}"
    );
    assert!(
        trait_count >= 1,
        "should find interface Serializable, got {trait_count}"
    );
    assert!(enum_count >= 1, "should find enum Color, got {enum_count}");
    assert!(
        type_alias_count >= 1,
        "should find type alias, got {type_alias_count}"
    );
    assert!(import_count >= 1, "should find imports, got {import_count}");
    assert!(const_count >= 1, "should find constant, got {const_count}");

    // Extends edge for Dog extends Animal
    let extends = result
        .refs
        .iter()
        .any(|r| r.kind == "Extends" && r.from_qualified_name.contains("Dog"));
    assert!(extends, "Dog should extend Animal");
}

#[test]
fn test_swift_parser_standalone() {
    // Regression guard for the ABI-15 defect: tree-sitter-swift 0.7.3 needs a
    // runtime supporting grammar ABI 15 (tree-sitter >= 0.25). If the runtime is
    // too old, `set_language` fails and `parse_swift_file` returns Err, so the
    // `.expect` below panics — this test fails loudly instead of Swift being
    // silently skipped by the indexer (the original defect). It also exercises
    // every member kind swift.rs classifies, which had no fixture before.
    // source: alex-pinkus/tree-sitter-swift v0.7.3 node kinds.
    let src = r#"
import Foundation
import UIKit

let maxRetries = 3

func greet(name: String) -> String {
    return "Hello, \(name)"
}

struct Point {
    let x: Int
    let y: Int

    func distance() -> Double {
        return compute(x, y)
    }
}

class Animal {
    var name: String
    init(name: String) { self.name = name }
    deinit { cleanup() }
    func speak() -> String { return "" }
    subscript(index: Int) -> Int { return index }
}

enum Color {
    case red
    case green, blue
}

protocol Serializable {
    func serialize() -> String
}

extension Animal {
    func describe() -> String { return name }
}

typealias Handler = () -> Void
"#;
    let result = parser::parse_file(src, "test.swift", Language::Swift)
        .expect("Swift parse should succeed (ABI 15 runtime required)");

    let fn_count = result
        .nodes
        .iter()
        .filter(|n| n.label == "Function")
        .count();
    let struct_count = result.nodes.iter().filter(|n| n.label == "Struct").count();
    let enum_count = result.nodes.iter().filter(|n| n.label == "Enum").count();
    let variant_count = result.nodes.iter().filter(|n| n.label == "Variant").count();
    let trait_count = result.nodes.iter().filter(|n| n.label == "Trait").count();
    let method_count = result.nodes.iter().filter(|n| n.label == "Method").count();
    let const_count = result
        .nodes
        .iter()
        .filter(|n| n.label == "Constant")
        .count();
    let import_count = result.nodes.iter().filter(|n| n.label == "Import").count();

    assert!(
        import_count >= 2,
        "should find imports (Foundation, UIKit), got {import_count}"
    );
    assert!(
        fn_count >= 1,
        "should find top-level func greet, got {fn_count}"
    );
    assert!(
        struct_count >= 2,
        "should find struct Point + class Animal (as Struct), got {struct_count}"
    );
    assert!(enum_count >= 1, "should find enum Color, got {enum_count}");
    assert!(
        trait_count >= 1,
        "should find protocol Serializable (as Trait), got {trait_count}"
    );
    assert!(
        variant_count >= 3,
        "should find enum cases red/green/blue, got {variant_count}"
    );
    assert!(method_count >= 3, "should find methods (distance/speak/init/deinit/subscript/describe/serialize), got {method_count}");
    assert!(
        const_count >= 3,
        "should find properties/typealias (x/y/name/maxRetries/Handler), got {const_count}"
    );

    // Call extraction: distance() calls compute, deinit calls cleanup.
    let has_call = result.refs.iter().any(|r| r.kind == "Calls");
    assert!(
        has_call,
        "should extract at least one Calls edge (compute/cleanup)"
    );
}
