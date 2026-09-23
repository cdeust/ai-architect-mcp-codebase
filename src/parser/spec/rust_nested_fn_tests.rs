// parser::spec::rust_nested_fn_tests — issue #327: a `fn` item declared inside
// a function body is a definition of its own, scoped under the enclosing
// function, and the calls in its body belong to it, not to the enclosing
// function.
//
// source: issue #327, measured on DYResearch/dy-wcet v4.1.2 `src/lib.rs:1081`
// (`fn gcd` inside `TaskSet::passes_utilisation_bound`): no node under any
// name, its call sites attributed to the enclosing method.

use crate::parser::{parse_file, ExtractedNode, Language, ParseResult};

const FILE: &str = "src/lib.rs";

fn parse(source: &str) -> ParseResult {
    parse_file(source, FILE, Language::Rust).expect("parse")
}

fn node<'a>(result: &'a ParseResult, label: &str, qn: &str) -> Option<&'a ExtractedNode> {
    result
        .nodes
        .iter()
        .find(|n| n.label == label && n.qualified_name == qn)
}

/// The `CallSite` nodes whose callee is `callee`, as (caller QN, line).
fn call_sites_of(result: &ParseResult, callee: &str) -> Vec<(String, u64)> {
    let prop = |n: &ExtractedNode, key: &str| {
        n.properties
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    result
        .nodes
        .iter()
        .filter(|n| n.label == "CallSite" && prop(n, "callee_name") == callee)
        .map(|n| (prop(n, "caller_qn"), n.start_line))
        .collect()
}

/// The issue's own shape: a method whose body declares a recursive helper and
/// calls it. Before the fix the helper had no node, and both `gcd(..)` sites
/// were attributed to `m`.
#[test]
fn fn_nested_in_a_method_body_is_indexed_and_owns_its_calls() {
    let source = "struct S; impl S { fn m(&self) -> u64 { \
                  fn gcd(a: u64, b: u64) -> u64 { if b == 0 { a } else { gcd(b, a % b) } } \
                  gcd(6, 4) } }";
    let result = parse(source);

    let nested = node(&result, "Function", "src/lib.rs::S::m::gcd")
        .expect("nested fn gcd must be a Function scoped under S::m");
    assert_eq!(nested.name, "gcd");
    assert!(
        node(&result, "Method", "src/lib.rs::S::m::gcd").is_none(),
        "a nested fn has no receiver: it is a Function, never a Method"
    );

    let mut callers: Vec<String> = call_sites_of(&result, "gcd")
        .into_iter()
        .map(|(caller, _)| caller)
        .collect();
    callers.sort();
    assert_eq!(
        callers,
        vec![
            "src/lib.rs::S::m".to_string(),
            "src/lib.rs::S::m::gcd".to_string()
        ],
        "gcd(6, 4) belongs to m; gcd(b, a % b) belongs to the nested gcd"
    );
    let outer_site = result
        .nodes
        .iter()
        .find(|n| n.label == "CallSite" && n.qualified_name.starts_with("src/lib.rs::S::m::call@"))
        .expect("m keeps its own call site");
    let at = source.find("gcd(6, 4)").expect("literal present");
    assert!(
        outer_site
            .qualified_name
            .ends_with(&format!("#{at}-{}", at + "gcd(6, 4)".len())),
        "the call site m owns is gcd(6, 4), got {}",
        outer_site.qualified_name
    );
}

/// A free function, a two-level nest and a fn declared inside a closure body:
/// each nested fn is scoped under the nearest enclosing fn item (a closure is
/// not a named scope, so its calls and items belong to the enclosing fn).
#[test]
fn fn_nested_in_free_fn_two_levels_and_closure_is_scoped_to_nearest_fn() {
    let source = r#"
fn free_outer(x: u64) -> u64 {
    fn helper(a: u64) -> u64 { a + 1 }
    helper(x)
}
fn two_levels() -> u64 {
    fn level1() -> u64 {
        fn level2() -> u64 { 7 }
        level2() + 1
    }
    level1()
}
fn with_closure() -> u64 {
    let f = || {
        fn in_closure() -> u64 { 3 }
        in_closure()
    };
    f()
}
"#;
    let result = parse(source);
    for qn in [
        "src/lib.rs::free_outer::helper",
        "src/lib.rs::two_levels::level1",
        "src/lib.rs::two_levels::level1::level2",
        "src/lib.rs::with_closure::in_closure",
    ] {
        assert!(node(&result, "Function", qn).is_some(), "missing {qn}");
    }
    let caller_of = |callee: &str| -> Vec<String> {
        call_sites_of(&result, callee)
            .into_iter()
            .map(|(c, _)| c)
            .collect()
    };
    assert_eq!(caller_of("helper"), vec!["src/lib.rs::free_outer"]);
    assert_eq!(caller_of("level2"), vec!["src/lib.rs::two_levels::level1"]);
    assert_eq!(caller_of("level1"), vec!["src/lib.rs::two_levels"]);
    assert_eq!(caller_of("in_closure"), vec!["src/lib.rs::with_closure"]);
}

/// The nested fn is defined by its file (the edge the graph schema carries
/// for a Function) and is never a harness entry point: rustc's
/// `unnameable_test_items` lint ("cannot test inner items") means a
/// `#[test]` on an inner fn is not collected by the test harness.
/// source: https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html#unnameable-test-items
#[test]
fn nested_fn_is_defined_by_its_file_and_carries_no_entry_kind() {
    let source = "fn outer() { #[test] fn inner() {} inner() }";
    let result = parse(source);
    let inner = node(&result, "Function", "src/lib.rs::outer::inner").expect("inner indexed");
    assert!(
        inner.properties.iter().all(|(k, _)| k != "entry_kind"),
        "an inner #[test] fn is not a harness entry: {:?}",
        inner.properties
    );
    assert!(result.refs.iter().any(|r| r.kind == "Defines"
        && r.from_qualified_name == FILE
        && r.to_qualified_name == "src/lib.rs::outer::inner"));
}

/// A fn-local `impl` is not a nested fn: its members are methods of a type
/// the walker does not index inside a function body, so none of them may be
/// emitted as a `Function` scoped under the enclosing fn.
#[test]
fn method_of_fn_local_impl_is_not_emitted_as_a_nested_function() {
    let source = "fn outer() { struct L; impl L { fn go(&self) {} } L.go() }";
    let result = parse(source);
    assert!(
        result
            .nodes
            .iter()
            .all(|n| n.label != "Function" || n.qualified_name == "src/lib.rs::outer"),
        "only outer is a Function: {:?}",
        result
            .nodes
            .iter()
            .filter(|n| n.label == "Function")
            .map(|n| &n.qualified_name)
            .collect::<Vec<_>>()
    );
}
