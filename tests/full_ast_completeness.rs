//! Completeness proof for full-AST persistence (issue: full-ast-index).
//!
//! `ai_architect_mcp::parser::ast_capture::capture_full_ast` claims to emit
//! ONE row per tree-sitter node — named and anonymous — for every parsed
//! file. This test holds that claim to an INDEPENDENT count: for each fixture
//! it parses the same source with the same grammar via a DIFFERENT tree-sitter
//! traversal API (`Node::child(i)` index iteration, not the `TreeCursor`
//! `capture_full_ast` itself walks with — both are tree-sitter API surfaces
//! guaranteed to iterate the identical child list, so this is a genuinely
//! separate code path, not a restatement of the function under test) and
//! asserts the two counts are EQUAL. A completeness ratio of exactly 1.0.
//!
//! Any drop, for any language, is a bug in `capture_full_ast` to fix — not a
//! limitation to document (owner directive, full-AST loop Phase 2).

use ai_architect_mcp::parser::ast_capture::capture_full_ast;
use ai_architect_mcp::parser::Language;

/// Recursively counts every tree-sitter node reachable from `node`, itself
/// included — named and anonymous alike. Uses `Node::child(i)` index
/// iteration deliberately: `capture_full_ast` walks with a `TreeCursor`
/// (`goto_first_child`/`goto_next_sibling`); tree-sitter guarantees both APIs
/// traverse the same child list in the same order, so this is an independent
/// verification, not shared implementation.
fn count_all_nodes_independently(node: tree_sitter::Node) -> usize {
    let mut count = 1;
    for i in 0..node.child_count() as u32 {
        if let Some(child) = node.child(i) {
            count += count_all_nodes_independently(child);
        }
    }
    count
}

/// Parses `source` directly with `ts_lang` (bypassing `capture_full_ast`
/// entirely) and returns the independent node count.
fn independent_count(source: &str, ts_lang: tree_sitter::Language) -> usize {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_lang).expect("set language");
    let tree = parser.parse(source, None).expect("parse");
    count_all_nodes_independently(tree.root_node())
}

/// One fixture: source text, the language `capture_full_ast` should use, and
/// the SAME grammar (selected independently — see each call site) for the
/// verification count.
struct Fixture {
    name: &'static str,
    source: &'static str,
    lang: Language,
    ts_lang: fn() -> tree_sitter::Language,
}

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            name: "python (file fixture)",
            source: include_str!("fixtures/multilang/sample.py"),
            lang: Language::Python,
            ts_lang: || tree_sitter_python::LANGUAGE.into(),
        },
        Fixture {
            name: "rust (file fixture)",
            source: include_str!("fixtures/multilang/sample.rs"),
            lang: Language::Rust,
            ts_lang: || tree_sitter_rust::LANGUAGE.into(),
        },
        Fixture {
            name: "typescript (file fixture)",
            source: include_str!("fixtures/multilang/sample.ts"),
            lang: Language::TypeScript,
            // `sample.ts` has no JSX, so the plain `typescript` grammar (not
            // `tsx`) is what `grammar_for`/`ts_language_by_ext` selects for a
            // `.ts` path — match that here for a fair independent count.
            ts_lang: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        },
        Fixture {
            name: "go",
            source: "package main\n\nfunc add(a int, b int) int {\n\treturn a + b\n}\n",
            lang: Language::Go,
            ts_lang: || tree_sitter_go::LANGUAGE.into(),
        },
        Fixture {
            name: "java",
            source: "public class Greeter {\n    public String greet(String name) {\n        return \"Hello, \" + name;\n    }\n}\n",
            lang: Language::Java,
            ts_lang: || tree_sitter_java::LANGUAGE.into(),
        },
        Fixture {
            name: "c",
            source: "int add(int a, int b) {\n    return a + b;\n}\n",
            lang: Language::C,
            ts_lang: || tree_sitter_c::LANGUAGE.into(),
        },
        Fixture {
            name: "ruby (shallow-spec fallback)",
            source: "def add(a, b)\n  a + b\nend\n",
            lang: Language::Ruby,
            ts_lang: || tree_sitter_ruby::LANGUAGE.into(),
        },
    ]
}

#[test]
fn completeness_ratio_is_1_0_across_languages() {
    let mut failures = Vec::new();
    for fx in fixtures() {
        let capture = capture_full_ast(fx.source, "fixture", fx.lang)
            .unwrap_or_else(|e| panic!("{}: capture_full_ast failed: {e}", fx.name));
        let expected = independent_count(fx.source, (fx.ts_lang)());
        let actual = capture.nodes.len();
        let ratio = actual as f64 / expected as f64;
        eprintln!(
            "{}: tree-sitter nodes = {expected}, persisted AstNode rows = {actual}, ratio = {ratio:.4}",
            fx.name
        );
        if actual != expected {
            failures.push(format!(
                "{}: persisted {actual} rows but the tree has {expected} nodes (ratio {ratio:.4}, not 1.0)",
                fx.name
            ));
        }
        // A clean, hand-written fixture should parse with zero ERROR/MISSING
        // nodes; a nonzero count here would mean the wrong grammar was
        // selected (or the fixture itself is malformed) — either way the
        // completeness comparison above would be comparing against a
        // degraded tree, so surface it explicitly rather than silently.
        assert_eq!(
            capture.parse_errors, 0,
            "{}: fixture parsed with {} ERROR/MISSING nodes — completeness check invalid",
            fx.name, capture.parse_errors
        );
    }
    assert!(
        failures.is_empty(),
        "completeness ratio must be exactly 1.0 for every language; failures:\n{}",
        failures.join("\n")
    );
}

/// Every non-root row's `(parent_id, child_index)` must be unique across the
/// whole capture (no two children of the same parent share an index) and the
/// tree must be reconstructible: replaying rows sorted by
/// `(parent_id, child_index)` must reproduce, for each parent, a contiguous
/// `0..N` run of child indices with no gaps.
#[test]
fn tree_is_exactly_reconstructible_from_parent_id_and_child_index() {
    for fx in fixtures() {
        let capture = capture_full_ast(fx.source, "fixture", fx.lang).unwrap();
        let mut children_by_parent: std::collections::HashMap<&str, Vec<u64>> =
            std::collections::HashMap::new();
        for row in &capture.nodes {
            if !row.parent_id.is_empty() {
                children_by_parent
                    .entry(row.parent_id.as_str())
                    .or_default()
                    .push(row.child_index);
            }
        }
        for (parent_id, mut indices) in children_by_parent {
            indices.sort_unstable();
            let expected: Vec<u64> = (0..indices.len() as u64).collect();
            assert_eq!(
                indices, expected,
                "{}: children of {parent_id} have non-contiguous child_index values",
                fx.name
            );
        }
    }
}
