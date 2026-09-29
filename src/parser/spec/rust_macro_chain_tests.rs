// parser::spec::rust_macro_chain_tests — a call reconstructed inside a macro
// argument keeps its whole receiver chain (issue #389).
//
// dy-wcet 8bb83ad tests/properties.rs:241 writes
// `assert!(build(task.wcet_us + extra).is_schedulable(), ..)` with `build` a
// local closure; the site was recorded as `(task.wcet_us + extra).is_schedulable`,
// so no receiver could ever be read from it. The same call written outside a
// macro is recorded with its full text, which is what these tests pin the
// macro scan to.

use crate::parser::{parse_file, Language};

/// The callee names of every call site under `caller`, in source order.
fn callees(src: &str, caller: &str) -> Vec<String> {
    let result = parse_file(src, "src/lib.rs", Language::Rust).expect("parse");
    assert_eq!(result.parse_errors, 0, "corpus must parse clean");
    let mut sites: Vec<(u64, String)> = result
        .nodes
        .iter()
        .filter(|n| n.label == "CallSite")
        .filter(|n| {
            n.properties
                .iter()
                .any(|(k, v)| k == "caller_qn" && v == caller)
        })
        .map(|n| (n.start_line, n.name.clone()))
        .collect();
    sites.sort();
    sites.into_iter().map(|(_, name)| name).collect()
}

#[test]
fn a_method_on_a_closure_call_keeps_the_closure_name() {
    let src = r#"
fn check(s: u32) {
    let build = |c: u32| c;
    assert!(build(s + 1).is_schedulable(), "fits");
    let direct = build(s + 1).is_schedulable();
}
"#;
    let calls = callees(src, "src/lib.rs::check");
    let in_macro = calls
        .iter()
        .filter(|c| *c == "build(s + 1).is_schedulable")
        .count();
    assert_eq!(
        in_macro, 2,
        "macro and plain sites must read alike, got {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with('(')),
        "no callee may start at the argument group, got {calls:?}"
    );
}

#[test]
fn a_method_on_a_method_result_keeps_the_whole_chain() {
    let src = r#"
fn check(s: Vec<u32>, i: usize) {
    assert!(s.get(i).is_some());
    assert_eq!(Set::new(3).len(), 0);
    assert!(vec![1].is_empty());
}
"#;
    let calls = callees(src, "src/lib.rs::check");
    for expected in ["s.get(i).is_some", "Set::new(3).len", "vec![1].is_empty"] {
        assert!(
            calls.iter().any(|c| c == expected),
            "{expected} missing from {calls:?}"
        );
    }
    for truncated in ["(i).is_some", "(3).len", "[1].is_empty"] {
        assert!(
            !calls.iter().any(|c| c == truncated),
            "{truncated} kept in {calls:?}"
        );
    }
}

#[test]
fn a_method_on_a_field_path_keeps_its_prefix() {
    let src = r#"
struct S { tasks: Vec<u32> }
impl S {
    fn check(&self, a: S) {
        assert!(self.tasks.is_empty());
        assert!(a.tasks.is_empty());
    }
}
"#;
    let calls = callees(src, "src/lib.rs::S::check");
    for expected in ["self.tasks.is_empty", "a.tasks.is_empty"] {
        assert!(
            calls.iter().any(|c| c == expected),
            "{expected} missing from {calls:?}"
        );
    }
    assert!(
        !calls.iter().any(|c| c == "tasks.is_empty"),
        "a field must not be read as a local receiver, got {calls:?}"
    );
}

#[test]
fn a_keyword_before_a_group_is_not_part_of_the_chain() {
    let src = r#"
fn check(n: u32) {
    println!("{:?}", for_each(n));
    assert!(matches!(n, x if (x).is_power_of_two()));
}
fn for_each(n: u32) -> u32 { n }
"#;
    let calls = callees(src, "src/lib.rs::check");
    assert!(
        calls.iter().any(|c| c == "(x).is_power_of_two"),
        "a group after a keyword stays its own receiver, got {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("if ")),
        "a keyword must not join the chain, got {calls:?}"
    );
}
