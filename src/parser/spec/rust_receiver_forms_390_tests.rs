// parser::spec::rust_receiver_forms_390_tests, issue #390: the receiver hint of
// `x.clone()`, of `if let Some(x) = f(..)` / `while let`, of a closure parameter
// and of a closure's return, and every shape of those that must NOT give one (a
// wrong single edge is worse than none).
//
// source: measured on DYResearch/dy-wcet v4.1.6 (commit 8bb83ad), seven calls of
// `TaskSet::is_schedulable` the 0.14.0 release leaves unresolved and
// rust-analyzer resolves.

use crate::parser::{parse_file, Language};

type Hint = (Option<String>, Option<String>);

/// `(receiver_hint, receiver_hint_via)` of the single `CallSite` whose callee
/// text is `callee`; each is `None` when the property is absent.
fn hint_of(src: &str, callee: &str) -> Hint {
    let result = parse_file(src, "src/lib.rs", Language::Rust).expect("parse");
    assert_eq!(result.parse_errors, 0, "corpus must parse clean: {src}");
    let site = result
        .nodes
        .iter()
        .find(|n| n.label == "CallSite" && n.name == callee)
        .unwrap_or_else(|| panic!("no CallSite with callee '{callee}' in {src:?}"));
    let prop = |key: &str| {
        site.properties
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    (prop("receiver_hint"), prop("receiver_hint_via"))
}

fn hint(ty: &str, via: &str) -> Hint {
    (Some(ty.into()), Some(via.into()))
}

const NONE: Hint = (None, None);

const SET: &str = "#[derive(Clone)]\n\
    struct Set { n: u8 }\n\
    impl Set {\n\
        fn new() -> Set { Set { n: 0 } }\n\
        fn ok(&self) -> bool { self.n > 0 }\n\
    }\n";

fn with_set(rest: &str) -> String {
    format!("{SET}{rest}")
}

// ---- form 1: `x.clone()` ---------------------------------------------------

#[test]
fn a_clone_of_self_in_an_impl_of_a_clone_type_types_the_receiver() {
    let src = with_set("impl Set { fn f(&self) -> bool { let t = self.clone(); t.ok() } }");
    assert_eq!(hint_of(&src, "t.ok"), hint("Set", "constructed"));
}

#[test]
fn a_clone_of_self_inside_a_closure_of_the_method_types_the_receiver() {
    let src = with_set(
        "impl Set { fn f(&self) -> bool { let g = |k: u8| { let t = self.clone(); t.ok() }; g(1) } }",
    );
    assert_eq!(hint_of(&src, "t.ok"), hint("Set", "constructed"));
}

#[test]
fn a_clone_keeps_the_type_and_the_evidence_of_the_cloned_binding() {
    let src = with_set("fn f() -> bool { let s = Set::new(); let c = s.clone(); c.ok() }");
    assert_eq!(hint_of(&src, "c.ok"), hint("Set", "assoc:new"));
    let twice = with_set(
        "fn f() -> bool { let s = Set::new(); let c = s.clone(); let d = c.clone(); d.ok() }",
    );
    assert_eq!(hint_of(&twice, "d.ok"), hint("Set", "assoc:new"));
    let typed = with_set("fn f(s: Set) -> bool { let c = s.clone(); c.ok() }");
    assert_eq!(hint_of(&typed, "c.ok"), (Some("Set".into()), None));
}

#[test]
fn a_type_with_a_hand_written_clone_impl_is_known_to_be_clone() {
    let src = "struct Set { n: u8 }\n\
        impl Clone for Set { fn clone(&self) -> Set { Set { n: self.n } } }\n\
        impl Set { fn ok(&self) -> bool { self.n > 0 } fn f(&self) -> bool { let t = self.clone(); t.ok() } }";
    assert_eq!(hint_of(src, "t.ok"), hint("Set", "constructed"));
}

#[test]
fn a_clone_of_a_type_not_known_to_be_clone_gives_no_hint() {
    let src = "struct Set { n: u8 }\n\
        impl Set { fn ok(&self) -> bool { self.n > 0 } fn f(&self) -> bool { let t = self.clone(); t.ok() } }";
    assert_eq!(hint_of(src, "t.ok"), NONE, "no derive and no impl: &Set");
    let conditional = "#[cfg_attr(feature = \"x\", derive(Clone))]\nstruct Set { n: u8 }\n\
        impl Set { fn ok(&self) -> bool { self.n > 0 } fn f(&self) -> bool { let t = self.clone(); t.ok() } }";
    assert_eq!(hint_of(conditional, "t.ok"), NONE, "derive under cfg_attr");
    let gated = "struct Set { n: u8 }\n#[cfg(feature = \"x\")]\n\
        impl Clone for Set { fn clone(&self) -> Set { Set { n: self.n } } }\n\
        impl Set { fn ok(&self) -> bool { self.n > 0 } fn f(&self) -> bool { let t = self.clone(); t.ok() } }";
    assert_eq!(hint_of(gated, "t.ok"), NONE, "impl under cfg");
    let generic = "#[derive(Clone)]\nstruct Set<T> { n: T }\n\
        impl<T> Set<T> { fn ok(&self) -> bool { true } fn f(&self) -> bool { let t = self.clone(); t.ok() } }";
    assert_eq!(hint_of(generic, "t.ok"), NONE, "generic type");
}

#[test]
fn a_clone_that_is_not_the_std_clone_gives_no_hint() {
    let inherent = "#[derive(Clone)]\nstruct Set { n: u8 }\n\
        impl Set { fn clone(&self) -> Other { Other } fn f(&self) -> bool { let t = self.clone(); t.ok() } }";
    assert_eq!(hint_of(inherent, "t.ok"), NONE, "inherent clone wins");
    let shadowed = "trait Clone { fn clone(&self) -> u8; }\n\
        #[derive(Clone)]\nstruct Set { n: u8 }\n\
        impl Set { fn ok(&self) -> bool { true } fn f(&self) -> bool { let t = self.clone(); t.ok() } }";
    assert_eq!(hint_of(shadowed, "t.ok"), NONE, "local trait named Clone");
    let wrapped = "use std::rc::Rc;\n#[derive(Clone)]\nstruct Set { n: u8 }\n\
        impl Set { fn ok(&self) -> bool { true } }\n\
        fn f(s: Rc<Set>) -> bool { let t = s.clone(); t.ok() }";
    assert_eq!(hint_of(wrapped, "t.ok"), NONE, "Rc<Set> clones the Rc");
}

#[test]
fn a_clone_of_an_untyped_or_rebound_receiver_gives_no_hint() {
    let unknown = with_set("fn f() -> bool { let c = mystery().clone(); c.ok() }");
    assert_eq!(hint_of(&unknown, "c.ok"), NONE);
    let untyped = with_set("fn f(s: Foreign) -> bool { let c = s.clone(); c.ok() }");
    assert_eq!(hint_of(&untyped, "c.ok"), NONE, "Foreign is no local type");
    let rebound = with_set(
        "impl Set { fn f(&self) -> bool { let t = self.clone(); let t = other(); t.ok() } }",
    );
    assert_eq!(hint_of(&rebound, "t.ok"), NONE);
    let args = with_set("impl Set { fn f(&self) -> bool { let t = self.clone_from(1); t.ok() } }");
    assert_eq!(hint_of(&args, "t.ok"), NONE, "not `clone`");
    let in_trait = "#[derive(Clone)]\nstruct Set;\ntrait Ok { fn ok(&self) -> bool; fn f(&self) -> bool { let t = self.clone(); t.ok() } }";
    assert_eq!(hint_of(in_trait, "t.ok"), NONE, "self of a trait default");
    let nested = with_set(
        "impl Set { fn f(&self) -> bool { fn inner() -> bool { let t = self.clone(); t.ok() } inner() } }",
    );
    assert_eq!(hint_of(&nested, "t.ok"), NONE, "self of another function");
}

// ---- form 2: `if let Some(x) = f(..)` / `while let` -------------------------

const BUILD: &str = "fn build() -> Option<Set> { Some(Set::new()) }\n\
    fn try_build() -> Result<Set, ()> { Ok(Set::new()) }\n";

fn with_build(rest: &str) -> String {
    format!("{SET}{BUILD}{rest}")
}

#[test]
fn if_let_some_of_a_call_types_the_binding_from_the_option() {
    let src = with_build("fn f() -> bool { if let Some(s) = build() { s.ok() } else { false } }");
    assert_eq!(hint_of(&src, "s.ok"), hint("Set", "return-type"));
}

#[test]
fn while_let_some_and_if_let_ok_type_the_binding() {
    let looped = with_build("fn f() { while let Some(s) = build() { s.ok(); } }");
    assert_eq!(hint_of(&looped, "s.ok"), hint("Set", "return-type"));
    let result =
        with_build("fn f() -> bool { if let Ok(s) = try_build() { s.ok() } else { false } }");
    assert_eq!(hint_of(&result, "s.ok"), hint("Set", "return-type"));
}

#[test]
fn if_let_types_the_binding_under_a_shadowing_of_the_name() {
    let src = with_build(
        "fn f() -> bool { let s = 1; if let Some(s) = build() { s.ok() } else { false } }",
    );
    assert_eq!(hint_of(&src, "s.ok"), hint("Set", "return-type"));
}

#[test]
fn if_let_gives_no_hint_when_the_option_is_not_provably_the_functions() {
    let wrong_wrapper =
        with_build("fn f() -> bool { if let Ok(s) = build() { s.ok() } else { false } }");
    assert_eq!(hint_of(&wrong_wrapper, "s.ok"), NONE, "Ok of an Option");
    let mapped = with_build(
        "fn f() -> bool { if let Some(s) = build().map(|x| x) { s.ok() } else { false } }",
    );
    assert_eq!(hint_of(&mapped, "s.ok"), NONE, "map changes the type");
    let unknown =
        with_build("fn f() -> bool { if let Some(s) = elsewhere() { s.ok() } else { false } }");
    assert_eq!(hint_of(&unknown, "s.ok"), NONE, "no such function here");
    let destructured =
        with_build("fn f() -> bool { if let Some((s, _)) = build() { s.ok() } else { false } }");
    assert_eq!(hint_of(&destructured, "s.ok"), NONE, "tuple pattern");
    let reference =
        with_build("fn f() -> bool { if let Some(ref s) = build() { s.ok() } else { false } }");
    assert_eq!(hint_of(&reference, "s.ok"), NONE, "ref pattern");
}

#[test]
fn if_let_gives_no_hint_outside_the_scope_of_the_binding() {
    let in_else =
        with_build("fn f(s: Set) -> bool { if let Some(s) = build() { true } else { s.ok() } }");
    assert_eq!(
        hint_of(&in_else, "s.ok"),
        (Some("Set".into()), None),
        "the parameter"
    );
    let only_here =
        with_build("fn f() -> bool { if let Some(s) = build() { true } else { s.ok() } }");
    assert_eq!(
        hint_of(&only_here, "s.ok"),
        NONE,
        "s is not bound in the else"
    );
    let after = with_build("fn f() -> bool { if let Some(s) = build() { s.ok(); } s.ok() }");
    assert_eq!(hint_of(&after, "s.ok"), NONE, "s is not bound after the if");
}

#[test]
fn if_let_gives_no_hint_when_the_function_is_shadowed_or_generic() {
    let shadowed = with_build(
        "fn f(build: u8) -> bool { if let Some(s) = build() { s.ok() } else { false } }",
    );
    assert_eq!(hint_of(&shadowed, "s.ok"), NONE, "build is a parameter");
    let generic = "fn make<T>() -> Option<T> { None }\n\
        fn f() -> bool { if let Some(s) = make() { s.ok() } else { false } }";
    assert_eq!(hint_of(generic, "s.ok"), NONE, "T is not a type");
}

// ---- form 3: a closure parameter -------------------------------------------

#[test]
fn a_closure_parameter_typed_in_the_closure_types_the_receiver() {
    let src = with_set("fn f() -> bool { let g = |s: Set| s.ok(); g(Set::new()) }");
    assert_eq!(hint_of(&src, "s.ok"), (Some("Set".into()), None));
}

#[test]
fn the_parameter_of_a_closure_given_to_an_option_method_takes_its_payload_type() {
    let src = with_build("fn f() -> bool { build().is_some_and(|s| s.ok()) }");
    assert_eq!(hint_of(&src, "s.ok"), hint("Set", "return-type"));
    let mapped = with_build("fn f() -> Option<bool> { build().map(|s| s.ok()) }");
    assert_eq!(hint_of(&mapped, "s.ok"), hint("Set", "return-type"));
    let result = with_build("fn f() -> bool { try_build().is_ok_and(|s| s.ok()) }");
    assert_eq!(hint_of(&result, "s.ok"), hint("Set", "return-type"));
}

#[test]
fn a_closure_parameter_gives_no_hint_when_its_type_is_not_proven() {
    let by_reference = with_build("fn f() -> Option<Set> { build().filter(|s| s.ok()) }");
    assert_eq!(
        hint_of(&by_reference, "s.ok"),
        NONE,
        "filter lends &T; not listed"
    );
    let wrong_method = with_build("fn f() -> bool { build().is_ok_and(|s| s.ok()) }");
    assert_eq!(
        hint_of(&wrong_method, "s.ok"),
        NONE,
        "is_ok_and is a Result method"
    );
    let two = with_build("fn f() -> bool { build().is_some_and(|s, t| s.ok()) }");
    assert_eq!(hint_of(&two, "s.ok"), NONE, "two parameters");
    let elsewhere = with_build("fn f() -> bool { other().is_some_and(|s| s.ok()) }");
    assert_eq!(hint_of(&elsewhere, "s.ok"), NONE, "no such function here");
    let stored = with_build("fn f(o: Option<Set>) -> bool { o.is_some_and(|s| s.ok()) }");
    assert_eq!(hint_of(&stored, "s.ok"), NONE, "receiver is not a call");
    let nested =
        with_build("fn f() -> bool { build().is_some_and(|x| { let g = |s| s.ok(); g(x) }) }");
    assert_eq!(
        hint_of(&nested, "s.ok"),
        NONE,
        "s belongs to the inner closure"
    );
}

// ---- form 4: the result of a local closure --------------------------------

#[test]
fn a_closure_returning_a_bound_constructor_types_the_call_on_its_result() {
    let src = with_set(
        "fn f() -> bool { let mk = |n: u8| { let mut t = Set::new(); t.n = n; t }; mk(1).ok() }",
    );
    assert_eq!(hint_of(&src, "mk(1).ok"), hint("Set", "assoc:new"));
}

#[test]
fn a_closure_returning_an_imported_binding_keeps_the_evidence_of_that_binding() {
    let src = "use dy::Set;\n\
        fn f() -> bool { let mk = |n: u8| { let mut t = Set::new(); t.n = n; t }; mk(1).ok() }";
    assert_eq!(hint_of(src, "mk(1).ok"), hint("Set", "assoc:new"));
    let unknown = "fn f() -> bool { let mk = |n: u8| { let t = mystery(n); t }; mk(1).ok() }";
    assert_eq!(hint_of(unknown, "mk(1).ok"), NONE);
}

#[test]
fn a_closure_returning_a_literal_or_declaring_its_return_type_types_the_call() {
    let literal = with_set("fn f() -> bool { let mk = |n: u8| Set { n }; mk(1).ok() }");
    assert_eq!(hint_of(&literal, "mk(1).ok"), hint("Set", "constructed"));
    let declared = with_set("fn f() -> bool { let mk = |n: u8| -> Set { make(n) }; mk(1).ok() }");
    assert_eq!(hint_of(&declared, "mk(1).ok"), hint("Set", "constructed"));
    let moved = with_set("fn f() -> bool { let mk = move |n: u8| Set { n }; mk(1).ok() }");
    assert_eq!(hint_of(&moved, "mk(1).ok"), hint("Set", "constructed"));
}

#[test]
fn a_closure_result_read_inside_a_macro_is_typed_the_same_way() {
    let src = with_set(
        "fn f() { let mk = |n: u8| { let mut t = Set::new(); t.n = n; t }; assert!(mk(1).ok()); }",
    );
    assert_eq!(hint_of(&src, "mk(1).ok"), hint("Set", "assoc:new"));
    let path =
        with_set("fn f() { let mk = |n: u8| { let t = Set::new(); t }; assert!(x::mk(1).ok()); }");
    assert_eq!(
        hint_of(&path, "x::mk(1).ok"),
        NONE,
        "a path is not the local"
    );
    let method =
        with_set("fn f() { let mk = |n: u8| { let t = Set::new(); t }; assert!(y.mk(1).ok()); }");
    assert_eq!(
        hint_of(&method, "y.mk(1).ok"),
        NONE,
        "a method is not the local"
    );
}

#[test]
fn a_closure_result_gives_no_hint_when_the_return_is_not_a_known_constructor() {
    let unknown = with_set("fn f() -> bool { let mk = |n: u8| make(n); mk(1).ok() }");
    assert_eq!(hint_of(&unknown, "mk(1).ok"), NONE, "make is unknown");
    let branches = with_set(
        "fn f() -> bool { let mk = |n: u8| if n > 1 { Set::new() } else { make(n) }; mk(1).ok() }",
    );
    assert_eq!(
        hint_of(&branches, "mk(1).ok"),
        NONE,
        "one branch is unknown"
    );
    // `new` returning an `Option`: the closure result carries exactly the hint of
    // the binding it returns (`assoc:new`), and the resolver drops that hint when
    // `new` does not build the type (issue #370), as it does for the binding.
    let not_new = "#[derive(Clone)]\nstruct Set { n: u8 }\n\
        impl Set { fn new() -> Option<Set> { None } fn ok(&self) -> bool { true } }\n\
        fn f() -> bool { let mk = |n: u8| { let t = Set::new(); t }; mk(1).ok() }\n\
        fn g() -> bool { let t = Set::new(); t.ok() }";
    assert_eq!(
        hint_of(not_new, "mk(1).ok"),
        hint_of(not_new, "t.ok"),
        "same evidence as the binding, no more"
    );
    let asynchronous = with_set("fn f() { let mk = async |n: u8| Set { n }; mk(1).ok() }");
    assert_eq!(hint_of(&asynchronous, "mk(1).ok"), NONE, "a future");
}

#[test]
fn a_closure_result_gives_no_hint_when_the_closure_is_not_the_one_the_call_names() {
    let rebound =
        with_set("fn f() -> bool { let mk = |n: u8| Set { n }; let mk = other; mk(1).ok() }");
    assert_eq!(hint_of(&rebound, "mk(1).ok"), NONE, "mk is rebound");
    let parameter = with_set("fn f(mk: fn(u8) -> Set) -> bool { mk(1).ok() }");
    assert_eq!(hint_of(&parameter, "mk(1).ok"), NONE, "mk is a parameter");
    let typed = with_set(
        "fn f() -> bool { let mk: Box<dyn Fn(u8) -> Other> = Box::new(|n| Other); mk(1).ok() }",
    );
    assert_eq!(hint_of(&typed, "mk(1).ok"), NONE, "mk is typed otherwise");
}
