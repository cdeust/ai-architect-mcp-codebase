// parser::spec::rust_return_type_tests — issues #348 and #349: the receiver
// hint read off a free function's declared return type, and every shape that
// must NOT give one (a wrong single edge is worse than none).

use crate::parser::{parse_file, Language};

/// `(receiver_hint, receiver_hint_via)` of the single `CallSite` whose callee
/// text is `callee`; each is `None` when the property is absent.
fn hint_of(src: &str, callee: &str) -> (Option<String>, Option<String>) {
    let result = parse_file(src, "src/lib.rs", Language::Rust).expect("parse");
    assert_eq!(result.parse_errors, 0, "corpus must parse clean");
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

fn derived(ty: &str) -> (Option<String>, Option<String>) {
    (Some(ty.to_string()), Some("return-type".to_string()))
}

const NONE: (Option<String>, Option<String>) = (None, None);

const SET: &str = "struct Set;\nimpl Set { fn m(&self) {} }\n";

fn with_set(rest: &str) -> String {
    format!("{SET}{rest}")
}

#[test]
fn a_plain_return_type_types_the_receiver_and_records_where_it_came_from() {
    let src = with_set("fn make() -> Set { Set }\nfn run() { let s = make(); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn a_type_written_at_the_binding_wins_and_is_not_marked() {
    let src = with_set("fn make() -> Set { Set }\nfn run() { let s: Set = make(); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), (Some("Set".into()), None));
}

#[test]
fn a_reference_return_type_is_stripped_to_the_named_type() {
    let src =
        with_set("fn make(x: &Set) -> &Set { x }\nfn run(x: &Set) { let s = make(x); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn an_option_is_unwrapped_by_let_else_expect_unwrap_and_the_question_mark() {
    let src = with_set(
        "fn build() -> Option<Set> { None }\n\
         fn a() { let Some(s) = build() else { return }; s.m(); }\n\
         fn b() { let s = build().expect(\"x\"); s.m(); }\n\
         fn c() { let s = build().unwrap(); s.m(); }\n\
         fn d() -> Option<()> { let s = build()?; s.m(); None }",
    );
    // One call site per function: the callee text is the same, so read each
    // function's own site through its position.
    let result = parse_file(&src, "src/lib.rs", Language::Rust).expect("parse");
    let hints: Vec<Option<String>> = result
        .nodes
        .iter()
        .filter(|n| n.label == "CallSite" && n.name == "s.m")
        .map(|n| {
            n.properties
                .iter()
                .find(|(k, _)| k == "receiver_hint_via")
                .map(|(_, v)| v.clone())
        })
        .collect();
    assert_eq!(hints.len(), 4);
    assert!(hints.iter().all(|h| h.as_deref() == Some("return-type")));
}

#[test]
fn a_result_is_unwrapped_by_let_ok_else_and_expect() {
    let src = with_set(
        "fn build() -> Result<Set, String> { Ok(Set) }\n\
         fn a() { let Ok(s) = build() else { return }; s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
    let src = with_set(
        "fn build() -> std::io::Result<Set> { Ok(Set) }\n\
         fn a() { let s = build().expect(\"x\"); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn an_option_used_without_unwrapping_gives_no_hint() {
    let src = with_set("fn build() -> Option<Set> { None }\nfn run() { let s = build(); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn the_wrong_pattern_for_the_wrapper_gives_no_hint() {
    let src = with_set(
        "fn build() -> Result<Set, String> { Ok(Set) }\n\
         fn run() { let Some(s) = build() else { return }; s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
    let src = with_set(
        "fn build() -> Option<Set> { None }\n\
         fn run() { let Ok(s) = build() else { return }; s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn unwrapping_a_plain_type_gives_no_hint() {
    let src = with_set("fn make() -> Set { Set }\nfn run() { let s = make().unwrap(); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_method_chain_after_the_call_is_not_seen_through() {
    let src = with_set(
        "fn build() -> Option<Set> { None }\n\
         fn run() { let s = build().unwrap_or_default(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_generic_or_impl_trait_or_dyn_return_type_gives_no_hint() {
    for ret in ["T", "impl Tr", "Box<dyn Tr>"] {
        let src = format!(
            "trait Tr {{ fn m(&self); }}\n\
             fn make<T: Default>() -> {ret} {{ todo!() }}\n\
             fn run() {{ let s = make(); s.m(); }}"
        );
        let hint = hint_of(&src, "s.m");
        assert!(
            hint == NONE || hint == derived("Box"),
            "{ret}: got {hint:?}; only the Box head may pass, and it finds no method"
        );
        if ret != "Box<dyn Tr>" {
            assert_eq!(hint, NONE, "{ret}");
        }
    }
}

#[test]
fn a_function_with_no_return_type_gives_no_hint() {
    let src = with_set("fn make() {}\nfn run() { let s = make(); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn two_free_functions_of_the_name_give_no_hint() {
    let src = with_set(
        "mod a { pub fn make() -> super::Set { super::Set } }\n\
         mod b { pub fn make() -> super::Set { super::Set } }\n\
         fn run() { let s = make(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_function_declared_in_a_module_that_does_not_enclose_the_call_gives_no_hint() {
    let src = with_set(
        "mod helpers { pub fn make() -> super::Set { super::Set } }\n\
         fn run() { let s = make(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_helper_in_the_enclosing_module_gives_a_hint() {
    let src = with_set(
        "mod tests {\n    use super::*;\n    fn make() -> Set { Set }\n\
         \x20   fn run() { let s = make(); s.m(); }\n}",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn a_method_of_the_name_does_not_count_as_a_second_free_function() {
    let src = with_set(
        "struct Other;\nimpl Other { fn make() -> Other { Other } }\n\
         fn make() -> Set { Set }\nfn run() { let s = make(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn a_use_that_names_the_function_gives_no_hint() {
    let src =
        with_set("use other::make;\nfn make() -> Set { Set }\nfn run() { let s = make(); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_local_or_parameter_of_the_callee_name_gives_no_hint() {
    let src =
        with_set("fn make() -> Set { Set }\nfn run(make: fn() -> Set) { let s = make(); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), NONE);
    let src = with_set(
        "fn make() -> Set { Set }\nfn run() { let make = || Set; let s = make(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_tuple_struct_or_const_of_the_callee_name_gives_no_hint() {
    let src =
        with_set("struct make(u8);\nfn make() -> Set { Set }\nfn run() { let s = make(); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_file_that_defines_its_own_option_gives_no_hint_through_it() {
    let src = with_set(
        "enum Option<T> { A(T), B }\nfn build() -> Option<Set> { Option::B }\n\
         fn run() { let s = build().expect(\"x\"); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_name_bound_twice_gives_no_hint() {
    let src =
        with_set("fn make() -> Set { Set }\nfn run() { let s = make(); let s = make(); s.m(); }");
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_qualified_callee_is_left_to_the_language_server() {
    // The pre-existing `T::assoc` reading takes the module `u` for a type
    // and gives the hint `u`, which finds no method; what this pass owns is
    // that it adds nothing of its own to a qualified callee.
    let src = with_set(
        "mod u { pub fn make() -> super::Set { super::Set } }\nfn run() { let s = u::make(); s.m(); }",
    );
    let (_, via) = hint_of(&src, "s.m");
    assert_eq!(via, None);
}

#[test]
fn a_call_inside_a_macro_argument_carries_the_same_hint() {
    let src =
        with_set("fn make() -> Set { Set }\nfn run() { let s = make(); assert_eq!(s.m(), ()); }");
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}
