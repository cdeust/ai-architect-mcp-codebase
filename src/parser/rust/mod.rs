// parser::rust — tree-sitter-based Rust source parser for code-intelligence graph.
//
// Parses a single `.rs` file and extracts typed symbols matching the
// graph_store schema. Zero dependency on graph_store or any storage layer.
//
// Grammar reference: https://github.com/tree-sitter/tree-sitter-rust

use tree_sitter::Parser;

use super::{ExtractedNode, ExtractedRef, ParseResult};

mod extract;

// ---------------------------------------------------------------------------
// Tree-sitter node type constants
// source: https://github.com/tree-sitter/tree-sitter-rust/blob/master/src/node-types.json
// ---------------------------------------------------------------------------

pub(crate) const TS_FUNCTION_ITEM: &str = "function_item";
pub(crate) const TS_FUNCTION_SIG: &str = "function_signature_item";
pub(crate) const TS_STRUCT_ITEM: &str = "struct_item";
pub(crate) const TS_ENUM_ITEM: &str = "enum_item";
pub(crate) const TS_ENUM_VARIANT: &str = "enum_variant";
pub(crate) const TS_TRAIT_ITEM: &str = "trait_item";
pub(crate) const TS_IMPL_ITEM: &str = "impl_item";
pub(crate) const TS_FIELD_DECL: &str = "field_declaration";
pub(crate) const TS_CONST_ITEM: &str = "const_item";
// source: tree-sitter-rust v0.23.3 — item kinds previously not dispatched.
// static_item is const-like (name+type); union_item is struct-like
// (name + field_declaration_list body); macro_definition / extern_crate carry a
// name field. All were silently dropped before.
pub(crate) const TS_STATIC_ITEM: &str = "static_item";
pub(crate) const TS_UNION_ITEM: &str = "union_item";
pub(crate) const TS_MACRO_DEFINITION: &str = "macro_definition";
pub(crate) const TS_EXTERN_CRATE: &str = "extern_crate_declaration";
pub(crate) const TS_TYPE_ITEM: &str = "type_item";
pub(crate) const TS_USE_DECL: &str = "use_declaration";
pub(crate) const TS_MOD_ITEM: &str = "mod_item";
pub(crate) const TS_VIS_MOD: &str = "visibility_modifier";
pub(crate) const TS_FUNC_MODS: &str = "function_modifiers";
pub(crate) const TS_DECL_LIST: &str = "declaration_list";
pub(crate) const TS_FIELD_DECL_LIST: &str = "field_declaration_list";
pub(crate) const TS_ENUM_VARIANT_LIST: &str = "enum_variant_list";
pub(crate) const TS_USE_AS_CLAUSE: &str = "use_as_clause";
pub(crate) const TS_USE_WILDCARD: &str = "use_wildcard";
pub(crate) const TS_CALL_EXPR: &str = "call_expression";
pub(crate) const TS_MACRO_INVOCATION: &str = "macro_invocation";
pub(crate) const TS_ATTRIBUTE_ITEM: &str = "attribute_item";
// Call-argument list plus the two node kinds a function passed *by value*
// (a higher-order argument) can take: a bare `identifier` (`map(process_order)`)
// or a `scoped_identifier` (`map(core::process_order)`). Used by the
// function-value-argument capture in extract::g4.
// source: https://github.com/tree-sitter/tree-sitter-rust node-types.json
//   (call_expression.arguments; identifier; scoped_identifier).
pub(crate) const TS_ARGUMENTS: &str = "arguments";
pub(crate) const TS_IDENTIFIER: &str = "identifier";
pub(crate) const TS_SCOPED_IDENTIFIER: &str = "scoped_identifier";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Parses a single `.rs` file and extracts typed symbols and relationships.
pub fn parse_rust_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set language: {e}"))?;
    let tree = super::parse_with_timeout(&mut parser, source)?;

    let mut ctx = ExtractCtx {
        source,
        file_path,
        nodes: Vec::new(),
        refs: Vec::new(),
    };
    extract::extract_top_level(&mut ctx, tree.root_node(), file_path);
    Ok(ParseResult {
        nodes: ctx.nodes,
        refs: ctx.refs,
        parse_errors: super::count_parse_errors(tree.root_node()),
        error_ranges: super::collect_error_ranges(tree.root_node()),
    })
}

// ---------------------------------------------------------------------------
// Extraction context
// ---------------------------------------------------------------------------

pub(crate) struct ExtractCtx<'a> {
    pub(crate) source: &'a str,
    pub(crate) file_path: &'a str,
    pub(crate) nodes: Vec<ExtractedNode>,
    pub(crate) refs: Vec<ExtractedRef>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_own_source() {
        let source =
            std::fs::read_to_string("src/main.rs").expect("should be able to read src/main.rs");
        let result = parse_rust_file(&source, "src/main.rs").expect("parse should succeed");

        let fn_names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.label == "Function")
            .map(|n| n.name.as_str())
            .collect();

        assert!(fn_names.contains(&"main"), "should find main()");
        assert!(
            fn_names.contains(&"handle_tool_call"),
            "should find handle_tool_call()"
        );
        assert!(
            fn_names.contains(&"write_message"),
            "should find write_message()"
        );
        assert!(
            result.nodes.len() > 30,
            "main.rs has dozens of items, got {}",
            result.nodes.len()
        );

        for node in &result.nodes {
            assert!(
                !node.name.is_empty(),
                "node with label {} has empty name",
                node.label
            );
        }
        for node in &result.nodes {
            if node.start_line > 0 {
                assert!(node.end_line >= node.start_line);
            }
        }
        assert!(
            !result.refs.is_empty(),
            "should have extracted some relationships"
        );
    }

    #[test]
    fn test_all_construct_types() {
        let src = r#"
pub async fn top_fn() {}
pub struct MyStruct { pub x: i32, y: String }
pub enum MyEnum { A, B }
pub trait MyTrait { fn method(&self); }
impl MyStruct { pub fn new() -> Self { todo!() } }
impl MyTrait for MyStruct { fn method(&self) {} }
const MAX: usize = 42;
type Alias = Vec<String>;
use std::collections::HashMap;
mod inner;
"#;
        let result = parse_rust_file(src, "test.rs").expect("parse");
        let labels: Vec<&str> = result.nodes.iter().map(|n| n.label.as_str()).collect();

        assert!(labels.contains(&"Function"), "missing Function");
        assert!(labels.contains(&"Struct"), "missing Struct");
        assert!(labels.contains(&"Enum"), "missing Enum");
        assert!(labels.contains(&"Variant"), "missing Variant");
        assert!(labels.contains(&"Trait"), "missing Trait");
        assert!(labels.contains(&"Method"), "missing Method");
        assert!(labels.contains(&"Field"), "missing Field");
        assert!(labels.contains(&"Constant"), "missing Constant");
        assert!(labels.contains(&"TypeAlias"), "missing TypeAlias");
        assert!(labels.contains(&"Import"), "missing Import");
        assert!(labels.contains(&"Module"), "missing Module");

        let top_fn = result.nodes.iter().find(|n| n.name == "top_fn").unwrap();
        let is_async_prop = top_fn
            .properties
            .iter()
            .find(|(k, _)| k == "is_async")
            .unwrap();
        assert_eq!(is_async_prop.1, "true");

        let x_field = result.nodes.iter().find(|n| n.name == "x").unwrap();
        let type_ann = x_field
            .properties
            .iter()
            .find(|(k, _)| k == "type_annotation")
            .unwrap();
        assert_eq!(type_ann.1, "i32");

        assert!(result
            .refs
            .iter()
            .any(|r| r.kind == "HasVariant" && r.from_qualified_name.contains("MyEnum")));
        assert!(result
            .refs
            .iter()
            .any(|r| r.kind == "HasField" && r.from_qualified_name.contains("MyStruct")));
        assert!(result.refs.iter().any(|r| r.kind == "HasMethod"));
    }

    #[test]
    fn test_visibility_extraction() {
        let src = r#"
pub fn public_fn() {}
pub(crate) fn crate_fn() {}
pub(super) fn super_fn() {}
fn private_fn() {}
"#;
        let result = parse_rust_file(src, "test.rs").expect("parse");
        let find = |name: &str| -> String {
            result
                .nodes
                .iter()
                .find(|n| n.name == name)
                .map(|n| n.visibility.clone())
                .unwrap_or_default()
        };
        assert_eq!(find("public_fn"), "pub");
        assert_eq!(find("crate_fn"), "pub(crate)");
        assert_eq!(find("super_fn"), "pub(super)");
        assert_eq!(find("private_fn"), "");
    }

    #[test]
    fn test_parse_multi_brace_use() {
        // Multi-brace `use` lists must expand into one Import per leaf so
        // that q9 (imports in file) and q14 (unresolved externals) can match
        // individual symbols — not the raw brace substring.
        let src = r#"
use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize as Ser};
use std::io::{self, BufRead};
use a::b::*;
use a::{b, c::{d, e}};
"#;
        let result = parse_rust_file(src, "test.rs").expect("parse");
        let import_names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.label == "Import")
            .map(|n| n.name.as_str())
            .collect();

        // HashMap / HashSet
        assert!(
            import_names.contains(&"std::collections::HashMap"),
            "missing std::collections::HashMap in {import_names:?}"
        );
        assert!(
            import_names.contains(&"std::collections::HashSet"),
            "missing std::collections::HashSet in {import_names:?}"
        );
        // alias: Serialize as Ser → display_name becomes the alias
        assert!(
            import_names.contains(&"Ser"),
            "aliased import should use alias as display name, got {import_names:?}"
        );
        // Deserialize is not aliased
        assert!(
            import_names.contains(&"serde::Deserialize"),
            "missing serde::Deserialize in {import_names:?}"
        );
        // `self` in brace list resolves to the prefix itself
        assert!(
            import_names.contains(&"std::io"),
            "missing std::io (from use std::io::{{self, ..}}) in {import_names:?}"
        );
        assert!(
            import_names.contains(&"std::io::BufRead"),
            "missing std::io::BufRead in {import_names:?}"
        );
        // nested brace list
        assert!(
            import_names.contains(&"a::b"),
            "missing a::b in {import_names:?}"
        );
        assert!(
            import_names.contains(&"a::c::d"),
            "missing a::c::d in {import_names:?}"
        );
        assert!(
            import_names.contains(&"a::c::e"),
            "missing a::c::e in {import_names:?}"
        );
        // Glob: display name ends in ::*
        assert!(
            import_names.contains(&"a::b::*"),
            "missing glob a::b::* in {import_names:?}"
        );
        // Regression: no entry should still contain a raw brace.
        for n in &import_names {
            assert!(
                !n.contains('{') && !n.contains('}'),
                "raw brace leaked into Import name: {n}"
            );
        }
    }

    #[test]
    fn test_impl_trait_property() {
        let src = r#"
trait MyTrait { fn do_it(&self); }
struct S;
impl MyTrait for S { fn do_it(&self) {} }
"#;
        let result = parse_rust_file(src, "test.rs").expect("parse");
        let method = result
            .nodes
            .iter()
            .find(|n| {
                n.label == "Method"
                    && n.name == "do_it"
                    && n.properties
                        .iter()
                        .any(|(k, v)| k == "trait_name" && v == "MyTrait")
            })
            .expect("should find impl method with trait_name property");
        assert!(method.qualified_name.contains("S"));
    }
}
