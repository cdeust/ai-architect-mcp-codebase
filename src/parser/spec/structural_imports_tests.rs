// parser::spec::structural_imports_tests — split out purely for the
// coding-standards.md §4.1 500-line file cap once `structural_imports.rs`'s
// TIER 2 import mechanism landed (mirrors why `structural_scope.rs` was
// split from `structural.rs`) — no new mechanism lives here, only the
// per-grammar verification tests for that module's public(-to-`spec`)
// surface.

use tree_sitter::Node;

use super::structural_imports::*;
use super::structural_imports_calls::import_call_path;

fn parse(lang: tree_sitter::Language, src: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).unwrap();
    parser.parse(src, None).unwrap()
}

fn anchors<'t>(root: Node<'t>) -> Vec<Node<'t>> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.is_named() && is_import_anchor(n) && !has_import_anchor_ancestor(n) {
            found.push(n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    found.sort_by_key(|n| n.start_byte());
    found
}

fn all_entry_paths(source: &str, root: Node) -> Vec<String> {
    let mut out = Vec::new();
    for anchor in anchors(root) {
        for entry in import_entries(anchor) {
            if let Some(p) = import_path_text(source, entry) {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn go_multi_spec_import_with_alias() {
    let src = "package main\nimport (\n\t\"fmt\"\n\tf \"os\"\n)\n";
    let tree = parse(tree_sitter_go::LANGUAGE.into(), src);
    let paths = all_entry_paths(src, tree.root_node());
    assert_eq!(paths, vec!["fmt", "os"]);
    let anchor = anchors(tree.root_node())[0];
    let entries = import_entries(anchor);
    assert_eq!(entries.len(), 2);
    assert_eq!(import_alias_text(src, entries[0]), None);
    assert_eq!(import_alias_text(src, entries[1]), Some("f".to_string()));
}

#[test]
fn python_import_from_and_aliased() {
    let src = "import os\nimport sys as s\nfrom foo import bar\n";
    let tree = parse(tree_sitter_python::LANGUAGE.into(), src);
    let paths = all_entry_paths(src, tree.root_node());
    assert_eq!(paths, vec!["os", "sys", "foo"]);
    let ancs = anchors(tree.root_node());
    let sys_entry = import_entries(ancs[1])[0];
    assert_eq!(import_alias_text(src, sys_entry), Some("s".to_string()));
}

#[test]
fn java_import_and_static_import() {
    let src = "import java.util.List;\nimport static java.lang.Math.PI;\n";
    let tree = parse(tree_sitter_java::LANGUAGE.into(), src);
    let paths = all_entry_paths(src, tree.root_node());
    assert_eq!(paths, vec!["java.util.List", "java.lang.Math.PI"]);
}

#[test]
fn rust_use_and_use_as() {
    let src = "use std::io;\nuse std::io as io2;\n";
    let tree = parse(tree_sitter_rust::LANGUAGE.into(), src);
    let paths = all_entry_paths(src, tree.root_node());
    assert_eq!(paths, vec!["std::io", "std::io"]);
    let ancs = anchors(tree.root_node());
    let aliased_entry = import_entries(ancs[1])[0];
    assert_eq!(
        import_alias_text(src, aliased_entry),
        Some("io2".to_string())
    );
}

#[test]
fn c_include_system_and_local() {
    let src = "#include <stdio.h>\n#include \"local.h\"\n";
    let tree = parse(tree_sitter_c::LANGUAGE.into(), src);
    let paths = all_entry_paths(src, tree.root_node());
    assert_eq!(paths, vec!["stdio.h", "local.h"]);
}

#[test]
fn cpp_include() {
    let src = "#include <vector>\n";
    let tree = parse(tree_sitter_cpp::LANGUAGE.into(), src);
    assert_eq!(all_entry_paths(src, tree.root_node()), vec!["vector"]);
}

#[test]
fn objc_import_directive() {
    let src = "#import <Foundation/Foundation.h>\n";
    let tree = parse(tree_sitter_objc::LANGUAGE.into(), src);
    assert_eq!(
        all_entry_paths(src, tree.root_node()),
        vec!["Foundation/Foundation.h"]
    );
}

#[test]
fn swift_plain_and_scoped_import() {
    let src = "import Foundation\nimport class UIKit.UIView\n";
    let tree = parse(tree_sitter_swift::LANGUAGE.into(), src);
    assert_eq!(
        all_entry_paths(src, tree.root_node()),
        vec!["Foundation", "UIKit.UIView"]
    );
}

#[test]
fn kotlin_ng_import() {
    let src = "import kotlin.io.println\nimport kotlin.io.*\n";
    let tree = parse(tree_sitter_kotlin_ng::LANGUAGE.into(), src);
    assert_eq!(
        all_entry_paths(src, tree.root_node()),
        vec!["kotlin.io.println", "kotlin.io.*"]
    );
}

#[test]
fn typescript_import_reads_the_source_field() {
    let src = "import { foo } from 'bar';\nimport baz from 'qux';\n";
    let tree = parse(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), src);
    assert_eq!(all_entry_paths(src, tree.root_node()), vec!["bar", "qux"]);
}

#[test]
fn ruby_require_reachable_only_via_the_call_path_not_a_keyword_anchor() {
    let src = "require 'set'\n";
    let tree = parse(tree_sitter_ruby::LANGUAGE.into(), src);
    // No keyword anchor at all: `require` is an ordinary call node in
    // Ruby's grammar, verified — this asserts the negative directly so a
    // future grammar change that DID add a keyword form would be caught.
    assert!(anchors(tree.root_node()).is_empty());
    let call = tree.root_node().named_child(0).unwrap();
    assert_eq!(call.kind(), "call");
    assert_eq!(import_call_path(src, call), Some("set".to_string()));
}

#[test]
fn lua_require_call_path() {
    let src = "local foo = require('foo')\n";
    let tree = parse(tree_sitter_lua::LANGUAGE.into(), src);
    // function_call is nested inside the local declaration's expression list.
    let mut found = None;
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_call" {
            found = Some(n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    let call = found.expect("function_call node");
    assert_eq!(import_call_path(src, call), Some("foo".to_string()));
}

#[test]
fn keyword_from_is_excluded_because_python_yield_from_also_carries_it() {
    let src = "def f():\n    yield from other()\n";
    let tree = parse(tree_sitter_python::LANGUAGE.into(), src);
    // `yield ... from ...` has a direct anonymous "from" child (verified
    // via tree dump during development) — asserting it is NOT flagged as
    // an import anchor pins the false-positive `"from"` would cause were
    // it added to `IMPORT_KEYWORD_TOKENS`.
    assert!(anchors(tree.root_node()).is_empty());
}

#[test]
fn csharp_using_directive_fieldless() {
    let src = "using System;\nusing System.Collections.Generic;\n";
    let tree = parse(tree_sitter_c_sharp::LANGUAGE.into(), src);
    assert_eq!(
        all_entry_paths(src, tree.root_node()),
        vec!["System", "System.Collections.Generic"]
    );
}

#[test]
fn php_use_require_and_include() {
    let src = "<?php\nuse Foo\\Bar;\nrequire 'file.php';\ninclude 'other.php';\n";
    let tree = parse(tree_sitter_php::LANGUAGE_PHP.into(), src);
    assert_eq!(
        all_entry_paths(src, tree.root_node()),
        vec!["Foo\\Bar", "file.php", "other.php"]
    );
}

#[test]
fn scala_import_single_and_selector() {
    let src = "import scala.collection.mutable\n";
    let tree = parse(tree_sitter_scala::LANGUAGE.into(), src);
    assert_eq!(
        all_entry_paths(src, tree.root_node()),
        vec!["scala.collection.mutable"]
    );
}

#[test]
fn haskell_import_with_alias() {
    let src = "import Data.List\nimport qualified Data.Map as Map\n";
    let tree = parse(tree_sitter_haskell::LANGUAGE.into(), src);
    assert_eq!(
        all_entry_paths(src, tree.root_node()),
        vec!["Data.List", "Data.Map"]
    );
    let ancs = anchors(tree.root_node());
    let aliased_entry = import_entries(ancs[1])[0];
    assert_eq!(
        import_alias_text(src, aliased_entry),
        Some("Map".to_string())
    );
}

#[test]
fn ocaml_open_module() {
    let src = "open Foo\nopen Bar.Baz\n";
    let tree = parse(tree_sitter_ocaml::LANGUAGE_OCAML.into(), src);
    assert_eq!(
        all_entry_paths(src, tree.root_node()),
        vec!["Foo", "Bar.Baz"]
    );
}

#[test]
fn dart_import_with_alias() {
    let src = "import 'dart:core';\nimport 'package:foo/foo.dart' as foo;\n";
    let tree = parse(tree_sitter_dart::LANGUAGE.into(), src);
    let paths = all_entry_paths(src, tree.root_node());
    assert_eq!(paths, vec!["dart:core", "package:foo/foo.dart"]);
    let ancs = anchors(tree.root_node());
    let aliased_entry = import_entries(ancs[1])[0];
    assert_eq!(
        import_alias_text(src, aliased_entry),
        Some("foo".to_string())
    );
}

#[test]
fn julia_using_and_import() {
    let src = "using Foo\nimport Bar\n";
    let tree = parse(tree_sitter_julia::LANGUAGE.into(), src);
    assert_eq!(all_entry_paths(src, tree.root_node()), vec!["Foo", "Bar"]);
}
