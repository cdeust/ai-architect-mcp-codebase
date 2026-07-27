// parser::spec::rust_walker_tests — the pre-migration unit tests, ported from
// the deleted `parser::rust::tests` and re-pointed at the public `parse_file`
// entry so the coverage they carried survives the migration (§14: deleting a
// test is a regression).
//
// These are behavior assertions on named constructs, complementary to the
// whole-file exact pin in `rust_parity_tests`: they name WHICH property matters
// (the async flag, the visibility text, the alias display name, the trait_name
// property) so a failure reads as a defect rather than a 61-record diff — and
// `rust_parses_own_source` is the only test in the suite that parses a real repo
// file, which is what catches a walker that works on a curated corpus and not on
// production Rust.

use crate::parser::{parse_file, Language};

/// The walker must read AP's own largest source file without dropping items.
#[test]
fn rust_parses_own_source() {
    let source = std::fs::read_to_string("src/main.rs").expect("read src/main.rs");
    let result = parse_file(&source, "src/main.rs", Language::Rust).expect("parse");

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
fn rust_all_construct_types_are_labelled() {
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
    let result = parse_file(src, "test.rs", Language::Rust).expect("parse");
    let labels: Vec<&str> = result.nodes.iter().map(|n| n.label.as_str()).collect();

    for expected in [
        "Function",
        "Struct",
        "Enum",
        "Variant",
        "Trait",
        "Method",
        "Field",
        "Constant",
        "TypeAlias",
        "Import",
        "Module",
    ] {
        assert!(labels.contains(&expected), "missing {expected}");
    }

    let top_fn = result.nodes.iter().find(|n| n.name == "top_fn").unwrap();
    assert_eq!(
        top_fn
            .properties
            .iter()
            .find(|(k, _)| k == "is_async")
            .unwrap()
            .1,
        "true"
    );
    let x_field = result.nodes.iter().find(|n| n.name == "x").unwrap();
    assert_eq!(
        x_field
            .properties
            .iter()
            .find(|(k, _)| k == "type_annotation")
            .unwrap()
            .1,
        "i32"
    );
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
fn rust_visibility_is_the_modifier_text() {
    let src = r#"
pub fn public_fn() {}
pub(crate) fn crate_fn() {}
pub(super) fn super_fn() {}
fn private_fn() {}
"#;
    let result = parse_file(src, "test.rs", Language::Rust).expect("parse");
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
fn rust_multi_brace_use_expands_to_atomic_imports() {
    // Multi-brace `use` lists must expand into one Import per leaf so that q9
    // (imports in file) and q14 (unresolved externals) can match individual
    // symbols — not the raw brace substring.
    let src = r#"
use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize as Ser};
use std::io::{self, BufRead};
use a::b::*;
use a::{b, c::{d, e}};
"#;
    let result = parse_file(src, "test.rs", Language::Rust).expect("parse");
    let import_names: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| n.label == "Import")
        .map(|n| n.name.as_str())
        .collect();

    for expected in [
        "std::collections::HashMap",
        "std::collections::HashSet",
        // alias: `Serialize as Ser` → the display name becomes the alias.
        "Ser",
        "serde::Deserialize",
        // `self` in a brace list resolves to the prefix itself.
        "std::io",
        "std::io::BufRead",
        "a::b",
        "a::c::d",
        "a::c::e",
        // Glob: the display name ends in `::*`.
        "a::b::*",
    ] {
        assert!(
            import_names.contains(&expected),
            "missing {expected} in {import_names:?}"
        );
    }
    for n in &import_names {
        assert!(
            !n.contains('{') && !n.contains('}'),
            "raw brace leaked into Import name: {n}"
        );
    }
}

#[test]
fn rust_trait_impl_method_carries_the_trait_name() {
    let src = r#"
trait MyTrait { fn do_it(&self); }
struct S;
impl MyTrait for S { fn do_it(&self) {} }
"#;
    let result = parse_file(src, "test.rs", Language::Rust).expect("parse");
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

/// The trait-requirement `is_async` split: a bodiless requirement is reported
/// NON-async unconditionally, while a defaulted `fn` has its modifiers read.
///
/// This test exists because §12 mutation testing found the split unasserted. The
/// captured parity corpus contains no `async` trait method, so the mutant
/// `delete ! in emit_trait_methods` — turning `!is_sig && has_async(..)` into
/// `is_sig && has_async(..)` — SURVIVED the entire suite: with no async trait
/// method anywhere, both forms evaluate to `false` for every corpus record. That
/// is a real coverage gap, not an equivalent mutant, so it is closed here rather
/// than documented away.
///
/// The rule asserted is the one the pre-migration `parser::rust::extract`
/// computed, read from its source before deletion:
///   `let is_async = if is_fn { has_async_modifier(child) } else { false };`
/// The corpus could not be re-captured to cover it (the walker it was captured
/// from is gone), so the coverage lands here as a behavior test instead.
///
/// Both directions are needed to kill the mutant and to pin the whole rule:
///   - `flush` is an `async`-marked SIGNATURE — the original reports `false`
///     (the signature branch short-circuits before the modifiers are read); the
///     mutant would report `true`.
///   - `drain` is an `async` DEFAULTED fn — the original reports `true`; the
///     mutant would report `false`.
///   - `plain` is a non-async defaulted fn — `false` either way, the control.
#[test]
fn rust_trait_requirement_async_flag_splits_signature_from_default() {
    let src = r#"
pub trait Flusher {
    async fn flush(&self);
    async fn drain(&self) {
        cleanup();
    }
    fn plain(&self) {}
}
"#;
    let result = parse_file(src, "t.rs", Language::Rust).expect("parse");
    assert_eq!(
        result.parse_errors, 0,
        "the async-trait-method corpus must parse clean, got {} errors at {:?}",
        result.parse_errors, result.error_ranges
    );
    let is_async_of = |name: &str| -> String {
        result
            .nodes
            .iter()
            .find(|n| n.label == "Method" && n.name == name)
            .unwrap_or_else(|| panic!("no Method named {name}"))
            .properties
            .iter()
            .find(|(k, _)| k == "is_async")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("{name} has no is_async property"))
    };
    assert_eq!(
        is_async_of("flush"),
        "false",
        "a bodiless trait requirement is reported non-async even when marked \
         `async` — the pre-migration walker's unconditional `false` for a \
         signature"
    );
    assert_eq!(
        is_async_of("drain"),
        "true",
        "a DEFAULTED async trait method has its modifiers read"
    );
    assert_eq!(
        is_async_of("plain"),
        "false",
        "control: a non-async defaulted trait method"
    );
    // #131: the defaulted `drain` body IS now scanned, so `cleanup()` yields a
    // call site keyed by `drain`'s own QN; the bodiless `flush` requirement yields
    // none. The pin is inverted here, not deleted.
    let cleanup = result
        .nodes
        .iter()
        .find(|n| n.label == "CallSite" && n.name == "cleanup")
        .expect("a defaulted trait body must be scanned for calls (#131)");
    assert_eq!(
        cleanup
            .properties
            .iter()
            .find(|(k, _)| k == "caller_qn")
            .map(|(_, v)| v.as_str()),
        Some("t.rs::Flusher::drain"),
        "the call site is keyed by the defaulted method's own QN"
    );
    assert!(
        !result.nodes.iter().any(|n| n.label == "CallSite"
            && n.properties
                .iter()
                .any(|(k, v)| k == "caller_qn" && v == "t.rs::Flusher::flush")),
        "a bodiless requirement (`flush`) must yield no call site (#131)"
    );
}

/// #130: a module-nested `impl`'s methods scope to the enclosing MODULE's QN, and
/// that QN is IDENTICAL to the one the type's own `Struct`/`Enum` node declares —
/// so `HasMethod` can never again point at a QN no node owns (acceptance
/// criterion 4). A file-level impl is unchanged (criterion 2), and a nested
/// GENERIC impl composes the module prefix with the verbatim generic type text
/// (criterion 3).
#[test]
fn rust_module_nested_impl_scopes_to_the_module_qn() {
    let src = r#"
struct Config;
impl Config {
    fn new() -> Self { Config }
}
pub mod helpers {
    pub struct Inner { pub flag: bool }
    impl Inner {
        fn toggle(&mut self) { self.flag = !self.flag; }
    }
    pub struct Wrapper<T>(T);
    impl<T> Wrapper<T> {
        fn get(&self) -> &T { &self.0 }
    }
}
"#;
    let result = parse_file(src, "src/lib.rs", Language::Rust).expect("parse");
    assert_eq!(result.parse_errors, 0, "corpus must parse clean");

    // The owning type's QN is read off its own definition node, then every one of
    // its members' receiver QN and its `HasMethod` source are asserted EQUAL to it
    // — the two can no longer drift.
    let type_qn = |name: &str| -> String {
        result
            .nodes
            .iter()
            .find(|n| (n.label == "Struct" || n.label == "Enum") && n.name == name)
            .unwrap_or_else(|| panic!("no type named {name}"))
            .qualified_name
            .clone()
    };
    let assert_owned = |method: &str, owner_qn: &str, expect_method_qn: &str| {
        let m = result
            .nodes
            .iter()
            .find(|n| n.label == "Method" && n.name == method)
            .unwrap_or_else(|| panic!("no method named {method}"));
        assert_eq!(m.qualified_name, expect_method_qn, "{method} QN");
        let receiver = m
            .properties
            .iter()
            .find(|(k, _)| k == "receiver_type")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            receiver,
            Some(owner_qn),
            "{method} receiver_type == owner QN"
        );
        assert!(
            result.refs.iter().any(|r| r.kind == "HasMethod"
                && r.from_qualified_name == owner_qn
                && r.to_qualified_name == expect_method_qn),
            "HasMethod edge from the owner QN {owner_qn} to {expect_method_qn}"
        );
    };

    // File-level impl: unchanged.
    assert_eq!(type_qn("Config"), "src/lib.rs::Config");
    assert_owned("new", "src/lib.rs::Config", "src/lib.rs::Config::new");
    // Module-nested plain impl: scoped to the module, equal to the Struct's QN.
    assert_eq!(type_qn("Inner"), "src/lib.rs::helpers::Inner");
    assert_owned(
        "toggle",
        "src/lib.rs::helpers::Inner",
        "src/lib.rs::helpers::Inner::toggle",
    );
    // Module-nested generic impl: module prefix + verbatim generic type text.
    assert_eq!(type_qn("Wrapper"), "src/lib.rs::helpers::Wrapper");
    assert_owned(
        "get",
        "src/lib.rs::helpers::Wrapper<T>",
        "src/lib.rs::helpers::Wrapper<T>::get",
    );
}

/// #131: nested constructs inside a defaulted trait method's body are reached on
/// exactly the same terms as inside an `impl` method — a closure body, a macro
/// invocation (callee keeps its `!`), and a function-value argument (#87) each
/// emit their call site, all keyed by the defaulted method's own QN
/// (acceptance criterion 3).
#[test]
fn rust_trait_default_body_reaches_nested_constructs() {
    let src = r#"
macro_rules! shout { ($x:expr) => { $x }; }
fn run_all(check: fn(&str) -> bool, arg: &str) -> bool { check(arg) }
fn validate(s: &str) -> bool { !s.is_empty() }
pub trait Handler {
    fn handle(&self, input: &str) -> bool;
    fn describe(&self, input: &str) -> bool {
        let f = |s: &str| s.len();
        shout!(f(input));
        run_all(validate, input)
    }
}
"#;
    let result = parse_file(src, "src/lib.rs", Language::Rust).expect("parse");
    assert_eq!(result.parse_errors, 0, "corpus must parse clean");

    let callee_under = |caller: &str| -> Vec<&str> {
        result
            .nodes
            .iter()
            .filter(|n| n.label == "CallSite")
            .filter(|n| {
                n.properties
                    .iter()
                    .any(|(k, v)| k == "caller_qn" && v == caller)
            })
            .map(|n| n.name.as_str())
            .collect()
    };
    let calls = callee_under("src/lib.rs::Handler::describe");
    // Closure body call, macro invocation (callee keeps `!`), and the two
    // function-value-argument sites of `run_all(validate, input)` (#87).
    for expected in ["s.len", "shout!", "run_all", "validate", "input"] {
        assert!(
            calls.contains(&expected),
            "defaulted body must reach {expected}, got {calls:?}"
        );
    }
    // The bodiless requirement `handle` still yields nothing.
    assert!(
        callee_under("src/lib.rs::Handler::handle").is_empty(),
        "a bodiless requirement yields no call site (#131)"
    );
}
