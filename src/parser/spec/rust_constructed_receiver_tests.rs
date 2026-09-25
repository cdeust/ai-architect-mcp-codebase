// parser::spec::rust_constructed_receiver_tests, issue #355: the receiver hint
// read off a tuple-struct constructor, a struct literal or `Type::assoc(..)`,
// and every shape that must NOT give one (a wrong single edge is worse than
// none).

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

fn built(ty: &str) -> Hint {
    (Some(ty.into()), Some("constructed".into()))
}

fn built_by_signature(ty: &str) -> Hint {
    (Some(ty.into()), Some("constructed-return-type".into()))
}

const NONE: Hint = (None, None);

const TIER: &str = "struct Tier(u8);\n\
    impl Tier {\n\
        fn new(n: u8) -> Tier { Tier(n) }\n\
        fn join(&self, o: &Tier) -> u8 { self.0 + o.0 }\n\
    }\n";

const NAMED: &str = "struct Named { n: u8 }\nimpl Named { fn get(&self) -> u8 { self.n } }\n";

fn tier(rest: &str) -> String {
    format!("{TIER}{rest}")
}

// ---- the five forms of the issue -----------------------------------------

#[test]
fn a_tuple_constructor_bound_by_let_types_the_receiver() {
    let src = tier("fn f() -> u8 { let t = Tier(1); t.join(&Tier(2)) }");
    assert_eq!(hint_of(&src, "t.join"), built("Tier"));
}

#[test]
fn a_struct_literal_bound_by_let_types_the_receiver() {
    let src = format!("{NAMED}fn f() -> u8 {{ let v = Named {{ n: 3 }}; v.get() }}");
    assert_eq!(hint_of(&src, "v.get"), built("Named"));
}

#[test]
fn a_tuple_constructor_written_in_place_types_the_receiver() {
    let src = tier("fn f() -> u8 { Tier(1).join(&Tier(2)) }");
    assert_eq!(hint_of(&src, "Tier(1).join"), built("Tier"));
}

#[test]
fn a_struct_literal_written_in_place_types_the_receiver() {
    let src = format!("{NAMED}fn f() -> u8 {{ Named {{ n: 3 }}.get() }}");
    assert_eq!(hint_of(&src, "Named { n: 3 }.get"), built("Named"));
}

#[test]
fn an_associated_function_returning_the_type_types_an_in_place_receiver() {
    let src = tier("fn f() -> u8 { Tier::new(1).join(&Tier(2)) }");
    assert_eq!(
        hint_of(&src, "Tier::new(1).join"),
        built_by_signature("Tier")
    );
}

#[test]
fn self_as_a_return_type_parses_as_a_plain_type_and_types_the_receiver() {
    let src = "struct Tier(u8);\n\
        impl Tier {\n\
            fn new(n: u8) -> Self { Tier(n) }\n\
            fn join(&self) -> u8 { self.0 }\n\
        }\n\
        fn f() -> u8 { Tier::new(1).join() }";
    assert_eq!(
        hint_of(src, "Tier::new(1).join"),
        built_by_signature("Tier")
    );
}

#[test]
fn an_enum_with_an_inherent_constructor_is_typed_like_a_struct() {
    let src = "enum Kind { A }\n\
        impl Kind {\n\
            fn make() -> Kind { Kind::A }\n\
            fn m(&self) {}\n\
        }\n\
        fn f() { Kind::make().m() }";
    assert_eq!(hint_of(src, "Kind::make().m"), built_by_signature("Kind"));
}

#[test]
fn a_type_written_at_the_binding_or_typed_by_new_keeps_its_earlier_source() {
    let src = tier("fn f() -> u8 { let t = Tier::new(1); t.join(&Tier(2)) }");
    assert_eq!(hint_of(&src, "t.join"), (Some("Tier".into()), None));
    let typed = tier("fn f() -> u8 { let t: Tier = make(); t.join(&Tier(2)) }");
    assert_eq!(hint_of(&typed, "t.join"), (Some("Tier".into()), None));
}

// ---- an associated function that is not provably `-> Self` ----------------

fn assoc_returning(ret: &str) -> String {
    format!(
        "struct Tier(u8);\n\
         impl Tier {{\n\
             fn make(n: u8) -> {ret} {{ todo!() }}\n\
             fn join(&self) -> u8 {{ self.0 }}\n\
         }}\n\
         fn f() {{ Tier::make(1).join() }}"
    )
}

#[test]
fn an_associated_function_returning_anything_else_gives_no_hint() {
    for ret in [
        "Option<Self>",
        "Option<Tier>",
        "Result<Self, ()>",
        "Box<Self>",
        "&'static Self",
        "u8",
        "(Self, u8)",
        "Vec<Self>",
    ] {
        assert_eq!(
            hint_of(&assoc_returning(ret), "Tier::make(1).join"),
            NONE,
            "{ret}"
        );
    }
}

#[test]
fn an_async_or_generic_or_trait_associated_function_gives_no_hint() {
    let async_fn = "struct Tier(u8);\n\
        impl Tier { async fn make() -> Self { Tier(0) } fn join(&self) {} }\n\
        fn f() { Tier::make().join() }";
    assert_eq!(hint_of(async_fn, "Tier::make().join"), NONE);
    let trait_impl = "struct Tier(u8);\n\
        impl Default for Tier { fn default() -> Self { Tier(0) } }\n\
        impl Tier { fn join(&self) {} }\n\
        fn f() { Tier::default().join() }";
    assert_eq!(hint_of(trait_impl, "Tier::default().join"), NONE);
    let generic_impl = "struct Tier<T>(T);\n\
        impl<T> Tier<T> { fn make(t: T) -> Self { Tier(t) } fn join(&self) {} }\n\
        fn f() { Tier::make(1).join() }";
    assert_eq!(hint_of(generic_impl, "Tier::make(1).join"), NONE);
}

#[test]
fn two_functions_of_that_name_or_none_in_this_file_give_no_hint() {
    let two = "struct Tier(u8);\n\
        impl Tier { fn make() -> Self { Tier(0) } fn join(&self) {} }\n\
        impl Tier { fn make() -> Self { Tier(1) } }\n\
        fn f() { Tier::make().join() }";
    assert_eq!(hint_of(two, "Tier::make().join"), NONE);
    let elsewhere = "struct Tier(u8);\n\
        fn f() { Tier::make().join() }";
    assert_eq!(hint_of(elsewhere, "Tier::make().join"), NONE);
}

// ---- constructors that are not a struct of this file ----------------------

#[test]
fn a_generic_struct_a_turbofish_and_an_enum_variant_give_no_hint() {
    let generic = "struct W<T>(T);\nimpl<T> W<T> { fn m(&self) {} }\nfn f() { W(1).m() }";
    assert_eq!(hint_of(generic, "W(1).m"), NONE);
    let turbofish = "struct W<T>(T);\nimpl<T> W<T> { fn m(&self) {} }\nfn f() { W::<u8>(1).m() }";
    assert_eq!(hint_of(turbofish, "W::<u8>(1).m"), NONE);
    let variant = "enum Kind { A(u8) }\nimpl Kind { fn m(&self) {} }\nfn f() { Kind::A(1).m() }";
    assert_eq!(hint_of(variant, "Kind::A(1).m"), NONE);
    let glob =
        "enum Kind { A(u8) }\nuse Kind::*;\nimpl Kind { fn m(&self) {} }\nfn f() { A(1).m() }";
    assert_eq!(hint_of(glob, "A(1).m"), NONE);
    let option = "fn f() { Some(1).m() }";
    assert_eq!(hint_of(option, "Some(1).m"), NONE);
    let braced_variant =
        "enum Kind { A { n: u8 } }\nimpl Kind { fn m(&self) {} }\nfn f() { Kind::A { n: 1 }.m() }";
    assert_eq!(hint_of(braced_variant, "Kind::A { n: 1 }.m"), NONE);
}

#[test]
fn self_as_a_constructor_or_a_literal_gives_no_hint() {
    let ctor =
        "struct Tier(u8);\nimpl Tier { fn m(&self) -> u8 { Self(1).n() } fn n(&self) -> u8 { 0 } }";
    assert_eq!(hint_of(ctor, "Self(1).n"), NONE);
    let literal = "struct Named { n: u8 }\nimpl Named { fn m(&self) -> u8 { Self { n: 1 }.n() } fn n(&self) -> u8 { 0 } }";
    assert_eq!(hint_of(literal, "Self { n: 1 }.n"), NONE);
}

#[test]
fn a_type_alias_a_rename_and_a_macro_generated_struct_give_no_hint() {
    let alias = tier("type T2 = Tier;\nfn f() { T2(1).join(&Tier(2)) }");
    assert_eq!(hint_of(&alias, "T2(1).join"), NONE);
    let rename = tier("use other::Tier as T2;\nfn f() { T2(1).join(&Tier(2)) }");
    assert_eq!(hint_of(&rename, "T2(1).join"), NONE);
    let generated = "make_tier!();\nfn f() { Tier(1).join() }";
    assert_eq!(hint_of(generated, "Tier(1).join"), NONE);
}

#[test]
fn a_tuple_constructor_of_a_braced_struct_and_a_literal_of_a_tuple_struct_give_no_hint() {
    let braced = format!("{NAMED}fn f() {{ Named(1).get() }}");
    assert_eq!(hint_of(&braced, "Named(1).get"), NONE);
    let tuple = tier("fn f() { Tier { 0: 1 }.join(&Tier(2)) }");
    assert_eq!(hint_of(&tuple, "Tier { 0: 1 }.join"), NONE);
}

// ---- the name may denote something else --------------------------------------

#[test]
fn a_function_a_const_a_static_or_a_use_of_the_name_gives_no_hint() {
    let function = format!("{NAMED}fn Named() {{}}\nfn f() {{ Named {{ n: 3 }}.get() }}");
    assert_eq!(hint_of(&function, "Named { n: 3 }.get"), NONE);
    let nested_fn = tier("fn f() { fn Tier(n: u8) -> u8 { n } Tier(1).join(&Tier(2)) }");
    assert_eq!(hint_of(&nested_fn, "Tier(1).join"), NONE);
    let constant = tier("const Tier: u8 = 1;\nfn f() { Tier(1).join(&Tier(2)) }");
    assert_eq!(hint_of(&constant, "Tier(1).join"), NONE);
    let stat = tier("static Tier: u8 = 1;\nfn f() { Tier(1).join(&Tier(2)) }");
    assert_eq!(hint_of(&stat, "Tier(1).join"), NONE);
    let import = tier("use elsewhere::Tier;\nfn f() { Tier(1).join(&Tier(2)) }");
    assert_eq!(hint_of(&import, "Tier(1).join"), NONE);
}

#[test]
fn a_local_binding_named_like_the_struct_gives_no_hint() {
    let src = tier("fn f(Tier: fn(u8) -> Tier) -> u8 { Tier(1).join(&Tier(2)) }");
    assert_eq!(hint_of(&src, "Tier(1).join"), NONE);
}

#[test]
fn two_structs_of_the_name_in_one_file_give_no_hint() {
    let src = "mod a { pub struct Tier(pub u8); impl Tier { pub fn join(&self) {} } }\n\
        mod b { pub struct Tier(pub u8); impl Tier { pub fn join(&self) {} } fn f() { Tier(1).join() } }";
    assert_eq!(hint_of(src, "Tier(1).join"), NONE);
}

// ---- scope: a struct the call cannot see gives no hint ------------------------

#[test]
fn a_struct_of_the_callers_module_is_seen_and_one_of_another_module_is_not() {
    let same =
        "mod m { struct Tier(u8); impl Tier { fn join(&self) {} } fn f() { Tier(1).join() } }";
    assert_eq!(hint_of(same, "Tier(1).join"), built("Tier"));
    let sibling = "mod a { pub struct Tier(pub u8); impl Tier { pub fn join(&self) {} } }\n\
        mod b { fn f() { Tier(1).join() } }";
    assert_eq!(hint_of(sibling, "Tier(1).join"), NONE);
    let parent = "struct Tier(u8);\nimpl Tier { fn join(&self) {} }\n\
        mod m { fn f() { Tier(1).join() } }";
    assert_eq!(hint_of(parent, "Tier(1).join"), NONE);
}

#[test]
fn a_child_module_sees_its_parents_struct_only_through_use_super_glob() {
    let glob = "struct Tier(u8);\nimpl Tier { fn join(&self) {} }\n\
        mod m { use super::*; fn f() { Tier(1).join() } }";
    assert_eq!(hint_of(glob, "Tier(1).join"), built("Tier"));
}

#[test]
fn a_struct_declared_in_the_enclosing_block_is_seen() {
    let src = "fn f() { struct Tier(u8); impl Tier { fn join(&self) {} } Tier(1).join() }";
    assert_eq!(hint_of(src, "Tier(1).join"), built("Tier"));
}

// ---- bindings: the live binding decides (issue #350) --------------------------

#[test]
fn a_shadowing_let_of_something_else_declines_and_a_later_constructor_types() {
    let untyped_later =
        tier("fn f(o: u8) -> u8 { let t = Tier(1); let t = other(); t.join(&Tier(2)) }");
    assert_eq!(hint_of(&untyped_later, "t.join"), NONE);
    let constructor_later =
        tier("fn f() -> u8 { let t = other(); let t = Tier(1); t.join(&Tier(2)) }");
    assert_eq!(hint_of(&constructor_later, "t.join"), built("Tier"));
}

#[test]
fn a_binding_that_is_not_a_plain_untyped_let_gives_no_hint() {
    let typed = tier("fn f() -> u8 { let t: Tier = Tier(1); t.join(&Tier(2)) }");
    assert_eq!(hint_of(&typed, "t.join"), (Some("Tier".into()), None));
    let destructured = tier("fn f() -> u8 { let (t, _u) = (Tier(1), 2); t.join(&Tier(2)) }");
    assert_eq!(hint_of(&destructured, "t.join"), NONE);
    let param = tier("fn f(t: u8) -> u8 { t.join(&Tier(2)) }");
    assert_eq!(hint_of(&param, "t.join"), NONE);
}

// ---- shapes that stay without a hint ------------------------------------------

#[test]
fn a_chained_field_or_indexed_receiver_still_gives_no_hint() {
    let field =
        tier("struct S { x: Tier }\nimpl S { fn f(&self) -> u8 { self.x.join(&Tier(2)) } }");
    assert_eq!(hint_of(&field, "self.x.join"), NONE);
    let chain = tier("fn f(x: &str) -> usize { x.trim().len() }");
    assert_eq!(hint_of(&chain, "x.trim().len"), NONE);
    let chained_on_built = tier("fn f() -> u8 { Tier(1).join(&Tier(2)).min(3) }");
    assert_eq!(
        hint_of(&chained_on_built, "Tier(1).join(&Tier(2)).min"),
        NONE
    );
}

#[test]
fn a_constructor_inside_a_macro_argument_stays_unresolved() {
    // The macro scan rebuilds `(1).join` from the token tree: no receiver to type.
    let src = tier("fn f() { assert_eq!(Tier(1).join(&Tier(2)), 3); }");
    assert_eq!(hint_of(&src, "(1).join"), NONE);
}
