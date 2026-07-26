// parser::spec::ruby — the first language added purely as a shallow spec row
// (ADR-0056, issue #60's Done criterion).
//
// This file is the proof the criterion is met: it is a data literal and
// nothing else. There is no walker, no conventions impl, no trait, and no
// conditional anywhere in the extraction path that mentions Ruby. Compare the
// deep-path languages, whose rows are accompanied by 143-265 lines of Rust.
//
// What Ruby gets, and what it does not (ADR-0056 decision 3):
//   - `Function`/`Method`/`Struct` definitions, `Calls` edges, and the
//     `Defines`/`HasMethod` structure that scopes methods to their class.
//   - NO visibility. Ruby's `private`/`protected` are method calls that change
//     parser state for subsequent definitions, not modifiers on a node; a
//     shallow row cannot see them, and guessing would write a false property.
//   - NO `Imports` edges: Ruby has no import statement node. `require` is an
//     ordinary `call`, so requires surface as `Calls` to `require` — real
//     information, honestly labelled, rather than a synthesised import.
// Both gaps are reported through the shallow-depth signal, not left silent.
//
// Every node kind and field below is real in tree-sitter-ruby's
// node-types.json (the spec guard asserts it).
// source: tree-sitter/tree-sitter-ruby 0.23.1 src/node-types.json
// (pinned in Cargo.lock).

use super::shallow::ShallowSpec;
use crate::parser::Language;

/// The Ruby shallow spec row.
///
/// `module` joins `class` in `class_node_kinds`: a Ruby module is a namespace
/// whose methods should scope to it, and `Struct` is the closest shallow label
/// the graph schema offers. `singleton_method` (`def self.foo`) is a method
/// regardless of nesting, so it uses `method_node_kinds` rather than relying
/// on enclosing scope.
///
/// `callee_field: Some("method")` is load-bearing: tree-sitter-ruby models
/// `foo.bar` as a `call` with fields `receiver` + `method`, so the call's first
/// named child is the RECEIVER. Without this field the walker would record
/// `foo` as the callee instead of `bar`.
pub(crate) static RUBY_SPEC: ShallowSpec = ShallowSpec {
    language: Language::Ruby,
    function_node_kinds: &["method"],
    method_node_kinds: &["singleton_method"],
    class_node_kinds: &["class", "module"],
    call_node_kinds: &["call"],
    // Ruby has no import statement node kind; `require` is a `call`.
    import_node_kinds: &[],
    callee_field: Some("method"),
    name_field: "name",
    body_field: Some("body"),
    ts_language: || tree_sitter_ruby::LANGUAGE.into(),
};
