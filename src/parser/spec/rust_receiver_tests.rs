// parser::spec::rust_receiver_tests — issue #283 palier 3 (lot 6): the
// parser-side `receiver_hint` property on a Rust CallSite.
//
// Split into its own file rather than appended to `rust_walker_tests.rs`
// (already at 481/500 lines — see coding-standards.md §4.1) — the same
// precedent `rust_call_shape_tests.rs` and `rust_parity_corpus.rs` set for
// this crate.
//
// source: tasks/plan-issues-282-283-284.md §2.4 (lot 6 test list).

use crate::parser::{parse_file, Language};

/// The `receiver_hint` property of the single `CallSite` whose `name`
/// (callee text) is `callee`, or `None` when no such property was attached
/// (either the site has no property by that key, or no matching site
/// exists — the two are collapsed deliberately: every assertion below names
/// the exact callee it expects, so a missing site is as much a failure as a
/// missing property).
fn hint_of(src: &str, callee: &str) -> Option<String> {
    let result = parse_file(src, "src/lib.rs", Language::Rust).expect("parse");
    assert_eq!(
        result.parse_errors, 0,
        "corpus must parse clean, got {} errors at {:?}",
        result.parse_errors, result.error_ranges
    );
    result
        .nodes
        .iter()
        .find(|n| n.label == "CallSite" && n.name == callee)
        .unwrap_or_else(|| panic!("no CallSite with callee '{callee}' in {src:?}"))
        .properties
        .iter()
        .find(|(k, _)| k == "receiver_hint")
        .map(|(_, v)| v.clone())
}

#[test]
fn constructor_let_binding_yields_the_constructed_type() {
    let src = r#"
struct TaskSet;
impl TaskSet {
    fn new() -> TaskSet { TaskSet }
    fn m(&self) {}
}
fn run() {
    let s = TaskSet::new();
    s.m();
}
"#;
    assert_eq!(hint_of(src, "s.m"), Some("TaskSet".to_string()));
}

#[test]
fn typed_reference_parameter_yields_its_type() {
    let src = r#"
struct TaskSet;
impl TaskSet {
    fn m(&self) {}
}
fn f(s: &TaskSet) {
    s.m();
}
"#;
    assert_eq!(hint_of(src, "s.m"), Some("TaskSet".to_string()));
}

#[test]
fn generic_let_type_annotation_strips_the_generic_parameter() {
    let src = r#"
struct Wrapper<T>(T);
impl<T> Wrapper<T> {
    fn m(&self) {}
}
fn run() {
    let s: Wrapper<u8> = Wrapper(0);
    s.m();
}
"#;
    assert_eq!(hint_of(src, "s.m"), Some("Wrapper".to_string()));
}

#[test]
fn untypable_initializer_yields_no_hint() {
    let src = r#"
fn make() -> i32 { 0 }
struct M;
impl M { fn m(&self) {} }
fn run() {
    let s = make();
    s.m();
}
"#;
    assert_eq!(hint_of(src, "s.m"), None);
}

#[test]
fn two_bindings_of_different_types_yield_no_hint() {
    let src = r#"
struct A;
struct B;
impl A { fn new() -> A { A } }
impl B { fn new() -> B { B } fn m(&self) {} }
fn run() {
    let s = A::new();
    let s = B::new();
    s.m();
}
"#;
    assert_eq!(hint_of(src, "s.m"), None);
}

#[test]
fn macro_reconstructed_call_site_carries_the_hint_too() {
    // `assert_eq!(s.slack_of(1), None)` never produces a `call_expression`
    // for `s.slack_of(1)` — tree-sitter does not expand macros — so this
    // exercises `rust_macro_calls`'s token_tree reconstruction path, not
    // `rust.rs::call_entry`'s real-call path.
    let src = r#"
struct TaskSet;
impl TaskSet {
    fn new() -> TaskSet { TaskSet }
    fn slack_of(&self, i: i32) -> Option<i32> { None }
}
fn run() {
    let s = TaskSet::new();
    assert_eq!(s.slack_of(1), None);
}
"#;
    assert_eq!(hint_of(src, "s.slack_of"), Some("TaskSet".to_string()));
}

#[test]
fn self_receiver_never_carries_a_local_binding_hint() {
    // Paliers 1-2 (lot 4) own `self.<m>` entirely; palier 3 must never
    // attach a hint to it (`self`'s node kind is `self`, never
    // `identifier`, so `receiver_identifier` structurally excludes it — this
    // pins that exclusion behaviorally).
    let src = r#"
struct TaskSet;
impl TaskSet {
    fn m(&self) {}
    fn total(&self) -> i32 {
        self.m();
        0
    }
}
"#;
    assert_eq!(hint_of(src, "self.m"), None);
}

#[test]
fn unclassifiable_receiver_shapes_carry_no_hint() {
    let src = r#"
use std::collections::HashMap;
struct TaskSet { tasks: HashMap<String, i32> }
impl TaskSet {
    fn response_of(&self, i: i32) -> i32 { i }
    fn chained(&self) -> Option<&i32> { self.tasks.get("x") }
}
fn total(sets: &[TaskSet], i: i32) -> i32 {
    sets[0].response_of(i)
}
fn len_of(x: &str) -> usize {
    x.trim().len()
}
"#;
    for callee in ["self.tasks.get", "sets[0].response_of", "x.trim().len"] {
        assert_eq!(hint_of(src, callee), None, "{callee} must carry no hint");
    }
}
