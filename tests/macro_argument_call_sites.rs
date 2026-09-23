// Regression tests: a macro invocation's opaque `token_tree` argument
// payload hides real method/path calls from `walk_calls`'s DFS, because
// tree-sitter does not expand macros — `assert_eq!(s.slack_of(1), None)`
// never produces a `call_expression` node for `s.slack_of(1)`. Measured on
// DYResearch/dy-wcet: 96 assert-family macro invocations, at least 46
// containing a real method call on the same line as the macro.
//
// Every assertion below FAILS on the pre-fix code (only the macro's own
// `CallSite`, e.g. `assert_eq!`, is emitted) and PASSES after the fix
// (`rust_macro_calls::macro_argument_call_entries`).
//
// Fixtures are dy-wcet's own three measured shapes, used verbatim.

use ai_architect_mcp::parser::{self, ExtractedNode, Language, ParseResult};

fn parse(src: &str, path: &str) -> ParseResult {
    parser::parse_file(src, path, Language::Rust).expect("parse should not hard-fail")
}

/// The `CallSite` whose `callee_name` property equals `callee` exactly, or
/// `None` if there is no such CallSite.
fn callsite<'a>(r: &'a ParseResult, callee: &str) -> Option<&'a ExtractedNode> {
    r.nodes.iter().find(|n| {
        n.label == "CallSite"
            && n.properties
                .iter()
                .any(|(k, v)| k == "callee_name" && v == callee)
    })
}

/// All CallSite callee names in a parse result (diagnostic helper).
fn callees(r: &ParseResult) -> Vec<&str> {
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
fn assert_eq_macro_argument_yields_a_speculative_call_site() {
    let src = "fn f(s: Sched) {\n    assert_eq!(s.slack_of(1), None);\n}\n";
    let r = parse(src, "a.rs");
    assert!(
        callsite(&r, "s.slack_of").is_some(),
        "assert_eq! macro argument `s.slack_of(1)` must yield a CallSite; \
         got callees {:?}",
        callees(&r)
    );
    assert!(
        callsite(&r, "assert_eq!").is_some(),
        "the macro invocation itself must still yield its own CallSite"
    );
    assert_eq!(
        callsite(&r, "s.slack_of").unwrap().start_line,
        2,
        "the reconstructed call-site must report its real source line"
    );
}

#[test]
fn assert_macro_argument_with_no_call_args_yields_a_speculative_call_site() {
    let src = "fn f(s: Sched) {\n    assert!(s.is_schedulable());\n}\n";
    let r = parse(src, "b.rs");
    assert!(
        callsite(&r, "s.is_schedulable").is_some(),
        "assert! macro argument `s.is_schedulable()` must yield a CallSite; \
         got callees {:?}",
        callees(&r)
    );
}

#[test]
fn assert_macro_argument_followed_by_a_message_yields_a_speculative_call_site() {
    let src = "fn f(arranged: Sched) {\n    assert!(arranged.is_schedulable(), \"Audsley proposed an ordering that misses\");\n}\n";
    let r = parse(src, "c.rs");
    assert!(
        callsite(&r, "arranged.is_schedulable").is_some(),
        "assert! macro argument before the trailing message must yield a \
         CallSite; got callees {:?}",
        callees(&r)
    );
    // The trailing string literal must not confuse the scan into emitting a
    // spurious call site for it or anything derived from it.
    assert_eq!(
        r.nodes.iter().filter(|n| n.label == "CallSite").count(),
        2,
        "exactly two CallSites expected (the macro itself + the one real \
         call); got callees {:?}",
        callees(&r)
    );
}

#[test]
fn bare_function_call_inside_a_macro_argument_is_extracted_as_a_bare_call() {
    // A single identifier immediately followed by a `(` group (`helper(x)`)
    // is a bare function call (issue #328: dy-wcet v4.1.2's
    // `assert_eq!(old_response_of(&s, 1), Some(9))` produced no CallSite).
    // It is emitted under its own name, never fused with its argument into a
    // receiver call. This test pinned the pre-#328 exclusion (1 CallSite);
    // the nested-macro look-alike it guarded against (`vec![...]`) is told
    // apart by the `!` between name and group, see
    // `rust_call_shape_tests::a_nested_macro_or_an_index_inside_a_macro_is_not_a_bare_call`.
    let src = "fn f(x: i32) {\n    assert!(helper(x));\n}\n";
    let r = parse(src, "d.rs");
    assert!(
        callsite(&r, "helper.x").is_none(),
        "a bare function call must not be fused with its argument; got callees {:?}",
        callees(&r)
    );
    assert_eq!(callees(&r), ["assert!", "helper"]);
}

#[test]
fn two_separate_macro_arguments_are_not_misidentified_as_a_method_call() {
    // `assert!(flag, format_error(ctx))` — `flag` is a bare bool condition
    // and `format_error(ctx)` is a SEPARATE message argument that happens to
    // be a function call. The two identifiers `flag` and `format_error` are
    // adjacent named children of the token_tree (comma is anonymous, just
    // like `.`/`::`), presenting the identical
    // `[identifier, identifier, token_tree]` shape as a genuine
    // `flag.format_error(...)` receiver call — but they are joined by `, `
    // in the source, never `.`/`::`, so the scan must reject the pair rather
    // than fabricate a callee out of two unrelated arguments.
    let src = "fn f(flag: bool, ctx: i32) {\n    assert!(flag, format_error(ctx));\n}\n";
    let r = parse(src, "f.rs");
    assert!(
        callsite(&r, "flag, format_error").is_none(),
        "two comma-separated macro arguments must not be fused into a \
         fabricated callee; got callees {:?}",
        callees(&r)
    );
    // `format_error(ctx)` is a bare function call, extracted under its own
    // name since issue #328; `flag` is a bound parameter and stays a value.
    assert_eq!(callees(&r), ["assert!", "format_error"]);
}

#[test]
fn nested_call_inside_a_macro_argument_call_is_also_found() {
    // `assert!(s.method(a.other()))` — the outer call's own reconstructed
    // argument token_tree must itself be scanned, recursively, for the inner
    // call.
    let src = "fn f(s: Sched, a: Sched) {\n    assert!(s.method(a.other()));\n}\n";
    let r = parse(src, "e.rs");
    assert!(
        callsite(&r, "s.method").is_some(),
        "the outer call `s.method(...)` must yield a CallSite; got callees {:?}",
        callees(&r)
    );
    assert!(
        callsite(&r, "a.other").is_some(),
        "the nested call `a.other()` inside the outer call's arguments must \
         also yield a CallSite; got callees {:?}",
        callees(&r)
    );
}
