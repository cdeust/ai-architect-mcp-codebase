// parser::spec::rust_code_context: what kind of code a Rust function or method
// is, from its own attributes and the `#[cfg]` that reaches it (issue #354).
//
// A function is `test` code when it carries a test attribute (`#[test]`,
// `#[tokio::test]` and the `test` macros of the other well-known async runtimes
// and frameworks, `#[rstest]`, `#[test_case]`, `#[wasm_bindgen_test]`), or when
// a `#[cfg]` that reaches it
// requires the bare option `test` (an item, `mod`, `impl` or `fn` under
// `#[cfg(test)]`, or an inner `#![cfg(test)]`). `#[bench]` marks `bench` code and
// `#[kani::proof]` marks `proof` code. A function declared inside the body of a
// marked function belongs to it: it is nameable only from that body.
//
// The answer comes from the source text alone, so it is a pure function of the
// file and a content-hash cache stays correct. Where the file sits in the
// Cargo package (`tests/`, `benches/`, `examples/`) is a different fact, read
// from `cargo metadata` by the indexer (`indexer::target_context`).
//
// The rule fails safe: a function gets a context only on positive evidence, and
// `cfg(any(test, ..))`, `cfg(not(test))` and `cfg_attr(test, ..)` give none.
// source: The Rust Reference, "Testing attributes" (`test`), "Conditional
// compilation" (`cfg`); the test/bench harnesses of `cargo test` and `cargo
// bench`; Kani reference, attributes (`kani::proof`).

use tree_sitter::Node;

use super::rust_cfg_gate::gate_requires_test;
use crate::parser::node_text;

pub(crate) const CONTEXT_TEST: &str = "test";
pub(crate) const CONTEXT_BENCH: &str = "bench";
pub(crate) const CONTEXT_PROOF: &str = "proof";

/// Attribute names that are test attributes by themselves, alone or qualified
/// by their own crate (`rstest`, `rstest::rstest`).
const TEST_NAMES: [&str; 3] = ["rstest", "test_case", "wasm_bindgen_test"];

/// Crates whose `test` attribute macro turns an (async) function into a test:
/// `#[tokio::test]`, `#[actix_web::test]`, ... Only these count: an attribute
/// `#[other::test]` from a crate this list does not know may be anything, and
/// calling production code a test would hide its callers from the production
/// count, so it stays unmarked (the direction that fails safe).
const TEST_MACRO_CRATES: [&str; 8] = [
    "tokio",
    "async_std",
    "actix_rt",
    "actix_web",
    "sqlx",
    "test_log",
    "futures_test",
    "smol_potat",
];

/// The segments of an attribute path (`kani :: proof` gives `["kani", "proof"]`,
/// whatever the spacing), or `None` for a path that starts with `crate`, `self`,
/// `super` or anything else that is not a plain identifier chain.
fn segments(source: &str, path: Node) -> Option<Vec<String>> {
    match path.kind() {
        "identifier" => Some(vec![node_text(source, path)]),
        "scoped_identifier" => {
            let mut all = segments(source, path.child_by_field_name("path")?)?;
            all.push(node_text(source, path.child_by_field_name("name")?));
            Some(all)
        }
        _ => None,
    }
}

/// What one `attribute_item` marks: `test`, `bench`, `proof` or nothing.
pub(crate) fn attribute_marker(source: &str, attribute_item: Node) -> Option<&'static str> {
    let attribute = attribute_item.named_child(0)?;
    if attribute.kind() != "attribute" {
        return None;
    }
    let segs = segments(source, attribute.named_child(0)?)?;
    let names: Vec<&str> = segs.iter().map(String::as_str).collect();
    let has_arguments = attribute.child_by_field_name("arguments").is_some()
        || attribute.child_by_field_name("value").is_some();
    match names.as_slice() {
        ["kani", "proof"] if !has_arguments => Some(CONTEXT_PROOF),
        ["bench"] if !has_arguments => Some(CONTEXT_BENCH),
        ["test"] => Some(CONTEXT_TEST),
        [name] if TEST_NAMES.contains(name) => Some(CONTEXT_TEST),
        [krate, name] if TEST_NAMES.contains(name) && krate == name => Some(CONTEXT_TEST),
        [krate, "test"] if TEST_MACRO_CRATES.contains(krate) => Some(CONTEXT_TEST),
        _ => None,
    }
}

/// The marker among the outer attributes of `item`, comments skipped. They stop
/// at the first sibling that is neither an attribute nor a comment, so an
/// attribute never leaks to the next item. `proof` wins over `test`, `test` over
/// `bench`.
fn own_marker(source: &str, item: Node) -> Option<&'static str> {
    let mut found: Option<&'static str> = None;
    let mut sibling = item.prev_named_sibling();
    while let Some(previous) = sibling {
        match previous.kind() {
            "attribute_item" => {
                found = stronger(found, attribute_marker(source, previous));
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sibling = previous.prev_named_sibling();
    }
    found
}

fn stronger(a: Option<&'static str>, b: Option<&'static str>) -> Option<&'static str> {
    let rank = |m: Option<&str>| match m {
        Some(CONTEXT_PROOF) => 3,
        Some(CONTEXT_TEST) => 2,
        Some(_) => 1,
        None => 0,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// The context of the function or method `item`: `test`, `bench`, `proof`, or
/// `None` when nothing proves it is not production code.
pub(crate) fn code_context(source: &str, item: Node) -> Option<&'static str> {
    let mut found = own_marker(source, item);
    let mut ancestor = item.parent();
    while let Some(node) = ancestor {
        if node.kind() == "function_item" {
            found = stronger(found, own_marker(source, node));
        }
        ancestor = node.parent();
    }
    if found.is_none() && gate_requires_test(source, item) {
        found = Some(CONTEXT_TEST);
    }
    found
}
