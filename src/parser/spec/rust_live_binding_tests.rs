// parser::spec::rust_live_binding_tests: issue #350. A name bound more than
// once types its receiver through the binding that is live at the call, and
// every shape that cannot prove which binding is live stays without a hint.

use crate::parser::{parse_file, Language};

type Hint = (Option<String>, Option<String>);

/// `(receiver_hint, receiver_hint_via)` of the site whose callee text is `callee`.
fn hint_of(src: &str, callee: &str) -> Hint {
    let result = parse_file(src, "src/lib.rs", Language::Rust).expect("parse");
    assert_eq!(result.parse_errors, 0, "corpus must parse clean: {src}");
    let site = result
        .nodes
        .iter()
        .find(|n| n.label == "CallSite" && n.name == callee)
        .unwrap_or_else(|| panic!("no CallSite '{callee}' in {src:?}"));
    let prop = |key: &str| {
        site.properties
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    (prop("receiver_hint"), prop("receiver_hint_via"))
}

/// A receiver bound by `Ty::new()`: the hint and the associated function the
/// resolver checks (issue #370).
fn ctor(ty: &str) -> Hint {
    (Some(ty.into()), Some("assoc:new".into()))
}

/// A receiver whose type is written at its parameter.
fn written(ty: &str) -> Hint {
    (Some(ty.into()), None)
}

fn from_return(ty: &str) -> Hint {
    (Some(ty.into()), Some("return-type".into()))
}

const NONE: Hint = (None, None);

const TYPES: &str = "struct A;\nstruct B;\nstruct Other;\n\
impl A { fn new() -> A { A } fn first(&self) {} fn second(&self) {} fn outer(&self) {} \
fn iter(&self) -> Vec<Other> { vec![] } }\n\
impl B { fn new() -> B { B } fn first(&self) {} fn second(&self) {} fn inner(&self) {} }\n\
impl Other { fn body(&self) {} fn inner(&self) {} fn other(&self) {} }\n\
fn make() -> A { A }\n";

fn body(text: &str) -> String {
    format!("{TYPES}{text}")
}

#[test]
fn two_bindings_of_the_same_type_resolve_each_site() {
    let src = body("fn run() { let s = A::new(); s.first(); let s = A::new(); s.second(); }");
    assert_eq!(hint_of(&src, "s.first"), ctor("A"));
    assert_eq!(hint_of(&src, "s.second"), ctor("A"));
}

#[test]
fn a_shadow_that_changes_the_type_types_each_site_by_its_own_binding() {
    let src = body("fn run() { let s = A::new(); s.first(); let s = B::new(); s.second(); }");
    assert_eq!(hint_of(&src, "s.first"), ctor("A"));
    assert_eq!(hint_of(&src, "s.second"), ctor("B"));
}

#[test]
fn a_shadow_in_a_block_that_has_ended_does_not_reach_the_later_call() {
    let src = body("fn run() { let s = A::new(); { let s = B::new(); s.inner(); } s.outer(); }");
    assert_eq!(hint_of(&src, "s.inner"), ctor("B"));
    assert_eq!(hint_of(&src, "s.outer"), ctor("A"));
}

#[test]
fn a_call_before_the_second_let_uses_the_first_binding() {
    let src = body("fn run() { let s = A::new(); s.first(); let s = B::new(); }");
    assert_eq!(hint_of(&src, "s.first"), ctor("A"));
}

#[test]
fn the_initialiser_of_a_rebinding_sees_the_earlier_binding() {
    let src = body("fn run() { let s = A::new(); let s = s.iter(); }");
    assert_eq!(hint_of(&src, "s.iter"), ctor("A"));
}

#[test]
fn a_typed_parameter_then_a_let_types_each_site_by_its_own_binding() {
    let src = body("fn run(s: &A) { s.first(); let s = B::new(); s.second(); }");
    assert_eq!(hint_of(&src, "s.first"), written("A"));
    assert_eq!(hint_of(&src, "s.second"), ctor("B"));
}

#[test]
fn a_closure_parameter_of_the_same_name_is_live_only_inside_the_closure() {
    let src = body(
        "fn run(v: Vec<Other>) { let s = A::new(); v.iter().for_each(|s| s.body()); s.outer(); }",
    );
    assert_eq!(hint_of(&src, "s.body"), NONE);
    assert_eq!(hint_of(&src, "s.outer"), ctor("A"));
}

#[test]
fn an_if_let_binding_is_live_in_its_consequence_and_not_in_the_else() {
    let src = body(
        "fn run(o: Option<Other>) { let s = A::new(); \
         if let Some(s) = o { s.inner(); } else { s.first(); } s.outer(); }",
    );
    assert_eq!(hint_of(&src, "s.inner"), NONE);
    assert_eq!(hint_of(&src, "s.first"), ctor("A"));
    assert_eq!(hint_of(&src, "s.outer"), ctor("A"));
}

#[test]
fn an_if_let_chain_binds_in_the_rest_of_the_condition_and_the_consequence() {
    let src = body(
        "fn run(o: Option<Other>) { let s = A::new(); \
         if let Some(s) = o && s.inner() { s.body(); } s.outer(); }",
    );
    assert_eq!(hint_of(&src, "s.inner"), NONE);
    assert_eq!(hint_of(&src, "s.body"), NONE);
    assert_eq!(hint_of(&src, "s.outer"), ctor("A"));
}

#[test]
fn a_while_let_binding_is_live_in_the_loop_body_only() {
    let src = body(
        "fn run(mut it: std::vec::IntoIter<Other>) { let s = A::new(); \
         while let Some(s) = it.next() { s.body(); } s.outer(); }",
    );
    assert_eq!(hint_of(&src, "s.body"), NONE);
    assert_eq!(hint_of(&src, "s.outer"), ctor("A"));
}

#[test]
fn a_match_arm_binding_is_live_in_its_own_arm_only() {
    let src = body(
        "fn run(o: Option<Other>) { let s = A::new(); \
         match o { Some(s) => s.body(), None => s.first() } }",
    );
    assert_eq!(hint_of(&src, "s.body"), NONE);
    assert_eq!(hint_of(&src, "s.first"), ctor("A"));
}

#[test]
fn a_for_pattern_is_live_in_the_body_and_not_in_the_iterated_value() {
    let src = body("fn run() { let s = A::new(); for s in s.iter() { s.body(); } }");
    assert_eq!(hint_of(&src, "s.iter"), ctor("A"));
    assert_eq!(hint_of(&src, "s.body"), NONE);
}

#[test]
fn a_let_after_the_call_inside_a_loop_body_does_not_reach_it() {
    let src = body(
        "fn run() { let s = A::new(); loop { s.first(); let s = B::new(); s.second(); break; } }",
    );
    assert_eq!(hint_of(&src, "s.first"), ctor("A"));
    assert_eq!(hint_of(&src, "s.second"), ctor("B"));
}

#[test]
fn a_binding_a_macro_may_make_keeps_the_name_without_a_hint() {
    let src = body(
        "macro_rules! bind { ($n:ident, $e:expr) => { let $n = $e; } }\n\
         fn run(o: B) { let s = A::new(); let s = A::new(); bind!(s, o); s.first(); }",
    );
    assert_eq!(hint_of(&src, "s.first"), NONE);
}

#[test]
fn a_destructuring_live_binding_gives_no_hint() {
    let src = body("fn run(o: (Other, u8)) { let s = A::new(); let (s, _n) = o; s.body(); }");
    assert_eq!(hint_of(&src, "s.body"), NONE);
}

#[test]
fn a_binding_of_a_nested_function_is_not_live_in_the_outer_one() {
    let src =
        body("fn run() { let s = A::new(); fn inner(s: B) { s.second(); } inner(B); s.first(); }");
    assert_eq!(hint_of(&src, "s.first"), ctor("A"));
    assert_eq!(hint_of(&src, "s.second"), written("B"));
}

#[test]
fn mixed_tiers_each_site_keeps_the_tier_of_its_own_binding() {
    let src = body("fn run() { let s = make(); s.first(); let s = A::new(); s.second(); }");
    assert_eq!(hint_of(&src, "s.first"), from_return("A"));
    assert_eq!(hint_of(&src, "s.second"), ctor("A"));
}

#[test]
fn the_return_type_tier_reads_the_live_binding_when_the_name_is_bound_twice() {
    let src = body("fn run() { let s = make(); let s = make(); s.first(); }");
    assert_eq!(hint_of(&src, "s.first"), from_return("A"));
}

#[test]
fn the_return_type_tier_gives_no_hint_when_the_live_binding_is_not_a_call() {
    let src = body("fn run(o: A) { let s = make(); let s = o; s.first(); }");
    assert_eq!(hint_of(&src, "s.first"), NONE);
}

#[test]
fn a_call_in_a_match_guard_is_declined() {
    let src = body("fn run(o: Option<u8>) { let s = A::new(); match o { Some(_n) if s.first() => {} _ => {} } }");
    assert_eq!(hint_of(&src, "s.first"), NONE);
}

/// The call the measured crate has at `tests/adversarial.rs:139`: the receiver
/// sits in `matches!` inside `assert!`, both macros whose token trees are read
/// as calls and neither of which binds a name.
#[test]
fn a_call_inside_assert_matches_resolves_through_the_live_binding() {
    let src = body(
        "fn run() { let s = A::new(); let _ = s.first(); let s = A::new(); \
         assert!(matches!(s.second(), ()), \"refused\"); }",
    );
    assert_eq!(hint_of(&src, "s.second"), ctor("A"));
}
