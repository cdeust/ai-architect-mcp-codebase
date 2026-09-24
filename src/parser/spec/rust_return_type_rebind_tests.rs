// parser::spec::rust_return_type_rebind_tests: issues #348 and #349, second and
// third review rounds: a name rebound by any binding form, item binders, the
// reach of a `let`, renamed return types and glob imports must give no hint.

use crate::parser::{parse_file, Language};

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

const REBIND_HEAD: &str =
    "struct Other;\nimpl Other { fn m(&self) {} }\nfn make() -> Set { Set }\n";

fn rebound_in(body: &str) -> (Option<String>, Option<String>) {
    let src = with_set(&format!("{REBIND_HEAD}{body}"));
    hint_of(&src, "s.m")
}

// ---- second review: renamed return types, item binders, block reach, gaps ----

#[test]
fn a_return_type_renamed_by_use_as_gives_no_hint() {
    let src = with_set(
        "use other::Real as Local;\nfn make() -> Local { todo!() }\n\
         fn run() { let s = make(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_return_type_imported_under_its_own_name_keeps_the_hint() {
    // The lookup is by that name, which is the type's own: this is the
    // `use dy_wcet::TaskSet` shape of the measured crate.
    let src = with_set(
        "use other::Set as _unused;\nuse dy::Set;\nfn make() -> Set { todo!() }\n\
         fn run() { let s = make(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn a_let_in_an_inner_block_does_not_type_a_call_after_the_block() {
    let body = "fn f() { { let s = make(); } s.m(); }";
    assert_eq!(rebound_in(body), NONE);
}

#[test]
fn a_call_before_its_let_is_not_typed_by_it() {
    let body = "fn f() { loop { s.m(); let s = make(); } }";
    assert_eq!(rebound_in(body), NONE);
}

#[test]
fn a_static_const_or_use_item_of_the_name_gives_no_hint() {
    for item in [
        "static s: Other = Other;",
        "const s: Other = Other;",
        "use other::s;",
    ] {
        let body = format!("fn f() {{ {item} {{ let s = make(); }} s.m(); }}");
        assert_eq!(rebound_in(&body), NONE, "{item}");
    }
}

#[test]
fn a_const_generic_of_the_name_gives_no_hint() {
    let body = "fn f<const s: usize>() { let s = make(); s.m(); }";
    assert_eq!(rebound_in(body), NONE);
}

// Gaps the second review found in the pattern tests.
#[test]
fn a_name_rebound_by_a_let_else_gives_no_hint() {
    let body = "fn f(o: Option<Other>) { let s = make(); let Some(s) = o else { return }; s.m(); }";
    assert_eq!(rebound_in(body), NONE);
}

#[test]
fn a_name_rebound_by_a_let_chain_gives_no_hint() {
    let body = "fn f(a: Option<Other>, b: Option<u8>) { let s = make(); \
                if let Some(s) = a && let Some(_n) = b { s.m(); } }";
    assert_eq!(rebound_in(body), NONE);
}

#[test]
fn a_match_guard_using_the_arms_binding_gives_no_hint() {
    let body = "fn f(o: Option<Other>) { let s = make(); \
                match o { Some(s) if { s.m(); true } => {} _ => {} } }";
    assert_eq!(rebound_in(body), NONE);
}

#[test]
fn a_name_rebound_by_an_or_pattern_gives_no_hint() {
    let body = "enum E { A(Other), B(Other) }\nfn f(e: E) { let s = make(); \
                match e { E::A(s) | E::B(s) => s.m() } }";
    assert_eq!(rebound_in(body), NONE);
}

#[test]
fn a_name_rebound_by_a_closure_pattern_parameter_gives_no_hint() {
    let body = "fn f(v: Vec<&Other>) { let s = make(); v.into_iter().for_each(|&s| s.m()); }";
    assert_eq!(rebound_in(body), NONE);
    let body =
        "fn f(v: Vec<(Other, u8)>) { let s = make(); v.into_iter().for_each(|(s, _n)| s.m()); }";
    assert_eq!(rebound_in(body), NONE);
}

#[test]
fn a_name_rebound_by_a_shorthand_field_pattern_gives_no_hint() {
    let body =
        "struct P { s: Other, n: u8 }\nfn f(p: P) { let s = make(); let P { s, .. } = p; s.m(); }";
    assert_eq!(rebound_in(body), NONE);
}

#[test]
fn matches_is_an_unknown_macro_and_its_names_are_declined() {
    let body = "fn f(o: Option<Other>) { let s = make(); let _ = matches!(o, Some(s) if s.m() == ()); s.m(); }";
    assert_eq!(rebound_in(body), NONE);
}

#[test]
fn a_binder_token_inside_a_known_macro_declines_the_names_of_that_macro() {
    for inner in [
        "for s in v {}",
        "match s { _ => 1 }",
        "if let Some(s) = o { s.m(); }",
        "v.iter().all(|s| s.m() == ())",
        "match o { Some(s) => s.m(), None => () }",
    ] {
        let body = format!(
            "fn f(v: Vec<Other>, o: Option<Other>) {{ let s = make(); assert!({{ {inner}; true }}); s.m(); }}"
        );
        assert_eq!(rebound_in(&body), NONE, "{inner}");
    }
}

// ---- third review: a glob import may hide a foreign type of the same name ----

/// A file with no `struct Set` of its own, so a return type `Set` can only come
/// from an import. `impl Set` lives in the shared header of `with_set`, so this
/// helper builds the source by hand.
fn without_local_set(rest: &str) -> String {
    format!("struct Other;\nimpl Other {{ fn m(&self) {{}} }}\n{rest}")
}

#[test]
fn a_return_type_that_may_come_from_a_glob_import_gives_no_hint() {
    for import in [
        "use ext::shapes::*;",
        "use ext::{shapes::*, other};",
        "use super::*;",
        "use crate::x::*;",
    ] {
        let src = without_local_set(&format!(
            "{import}\nfn make() -> Set {{ todo!() }}\nfn run() {{ let s = make(); s.m(); }}"
        ));
        assert_eq!(hint_of(&src, "s.m"), NONE, "{import}");
    }
}

#[test]
fn a_return_type_defined_in_the_file_keeps_the_hint_beside_a_glob() {
    let src = with_set(
        "use ext::shapes::*;\nfn make() -> Set { Set }\nfn run() { let s = make(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn use_super_glob_in_a_tests_module_keeps_the_hint_for_a_type_of_the_same_file() {
    let src = with_set(
        "mod tests {\n    use super::*;\n    fn make() -> Set { Set }\n\
         \x20   fn run() { let s = make(); s.m(); }\n}",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn a_file_without_a_glob_keeps_the_hint_for_an_imported_type_until_a_crate_is_shown() {
    let src = without_local_set(
        "use dy::Set;\nfn make() -> Set { todo!() }\nfn run() { let s = make(); s.m(); }",
    );
    // The hint is recorded, marked unverified with the crate the path starts
    // with; the indexer promotes it only for a crate of the repository.
    assert_eq!(
        hint_of(&src, "s.m"),
        (Some("Set".into()), Some("return-type-import:dy".into()))
    );
}

// ---- fourth review: a definition counts only in the module of the function ----

#[test]
fn a_homonym_in_a_nested_module_does_not_shadow_a_glob_of_the_function_module() {
    let src = without_local_set(
        "use ext::shapes::*;\nmod inner { pub struct Set; }\n\
         fn make() -> Set { todo!() }\nfn run() { let s = make(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
}

#[test]
fn a_definition_in_the_module_of_the_function_beside_a_glob_still_resolves() {
    let src = with_set(
        "use ext::shapes::*;\nfn make() -> Set { Set }\nfn run() { let s = make(); s.m(); }",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn a_function_in_a_module_with_its_type_and_a_glob_at_the_root_still_resolves() {
    let src = without_local_set(
        "use ext::shapes::*;\nmod m {\n    pub struct Set;\n    impl Set { pub fn m(&self) {} }\n\
         \x20   pub fn make() -> Set { Set }\n    pub fn run() { let s = make(); s.m(); }\n}",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}

#[test]
fn a_type_of_the_root_reaches_a_module_only_through_an_import() {
    // By Rust's rule a bare `Set` inside `mod m` does not see the root's `Set`.
    let body = |import: &str| {
        with_set(&format!(
            "mod m {{\n    {import}\n    pub fn make() -> Set {{ Set }}\n\
             \x20   pub fn run() {{ let s = make(); s.m(); }}\n}}"
        ))
    };
    assert_eq!(hint_of(&body("use super::Set;"), "s.m"), derived("Set"));
    assert_eq!(hint_of(&body("use super::*;"), "s.m"), derived("Set"));
    // Nothing shows where the name comes from: not valid Rust, so not a guess.
    assert_eq!(hint_of(&body(""), "s.m"), NONE);
}

#[test]
fn a_glob_of_the_root_does_not_reach_a_module_that_globs_super() {
    // The root defines no `Set` and globs a foreign crate; `use super::*` in
    // `mod m` brings that glob, so the name may be foreign.
    let src = without_local_set(
        "use ext::shapes::*;\nmod m {\n    use super::*;\n    pub fn make() -> Set { todo!() }\n\
         \x20   pub fn run() { let s = make(); s.m(); }\n}",
    );
    assert_eq!(hint_of(&src, "s.m"), NONE);
    // With a definition at the root it shadows that glob.
    let src = with_set(
        "use ext::shapes::*;\nmod m {\n    use super::*;\n    pub fn make() -> Set { Set }\n\
         \x20   pub fn run() { let s = make(); s.m(); }\n}",
    );
    assert_eq!(hint_of(&src, "s.m"), derived("Set"));
}
