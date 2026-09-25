// parser::spec::rust_code_context_tests: issue #354. A function or method is
// `test`, `bench` or `proof` code only on positive evidence from its own
// attributes or the `#[cfg]` that reaches it; production code gets no property.

use crate::parser::{parse_file, Language, ParseResult};

fn parse(source: &str) -> ParseResult {
    parse_file(source, "src/lib.rs", Language::Rust).expect("parse")
}

fn prop(result: &ParseResult, name: &str, key: &str) -> String {
    result
        .nodes
        .iter()
        .find(|n| (n.label == "Function" || n.label == "Method") && n.name == name)
        .unwrap_or_else(|| panic!("no function or method named {name}"))
        .properties
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

fn context(source: &str, name: &str) -> String {
    prop(&parse(source), name, "code_context")
}

#[test]
fn every_test_attribute_marks_test_code_and_an_entry() {
    for attribute in [
        "#[test]",
        "#[tokio::test]",
        "#[tokio::test(flavor = \"multi_thread\")]",
        "#[async_std::test]",
        "#[rstest]",
        "#[test_case(1, 2)]",
        "#[wasm_bindgen_test]",
        "#[actix_web::test]",
        "#[sqlx::test]",
        "#[tokio :: test]",
        "#[rstest::rstest]",
    ] {
        let source = format!("{attribute}\nasync fn t() {{}}");
        let result = parse(&source);
        assert_eq!(prop(&result, "t", "code_context"), "test", "{attribute}");
        assert_eq!(prop(&result, "t", "entry_kind"), "test", "{attribute}");
    }
}

#[test]
fn bench_and_proof_attributes_mark_their_code() {
    let result =
        parse("#[bench]\nfn b() {}\n#[kani::proof]\nfn p() {}\n#[kani :: proof]\nfn q() {}");
    assert_eq!(prop(&result, "b", "code_context"), "bench");
    assert_eq!(prop(&result, "b", "entry_kind"), "");
    assert_eq!(prop(&result, "p", "code_context"), "proof");
    assert_eq!(prop(&result, "p", "entry_kind"), "proof");
    assert_eq!(prop(&result, "q", "code_context"), "proof");
    assert_eq!(prop(&result, "q", "entry_kind"), "proof");
}

#[test]
fn production_code_carries_no_property() {
    let result = parse("pub fn prod() {}\nstruct S;\nimpl S { fn m(&self) {} }");
    for name in ["prod", "m"] {
        let node = result.nodes.iter().find(|n| n.name == name).expect("node");
        assert!(
            node.properties.iter().all(|(k, _)| k != "code_context"),
            "{name}"
        );
    }
}

#[test]
fn a_helper_inside_a_cfg_test_module_is_test_code_without_an_entry() {
    let source = "#[cfg(test)]\nmod tests {\n fn helper() {}\n #[test]\n fn t() { helper(); }\n}";
    let result = parse(source);
    assert_eq!(prop(&result, "helper", "code_context"), "test");
    assert_eq!(prop(&result, "helper", "entry_kind"), "");
}

#[test]
fn a_method_of_a_cfg_test_impl_and_an_inner_cfg_test_file_are_test_code() {
    assert_eq!(
        context("struct S;\n#[cfg(test)]\nimpl S { fn m(&self) {} }", "m"),
        "test"
    );
    assert_eq!(context("#![cfg(test)]\nfn f() {}", "f"), "test");
}

#[test]
fn an_all_predicate_that_includes_test_still_requires_test() {
    let source = "#[cfg(all(test, feature = \"x\"))]\nfn f() {}";
    assert_eq!(context(source, "f"), "test");
}

#[test]
fn predicates_that_do_not_require_test_give_no_context() {
    for gate in [
        "#[cfg(any(test, feature = \"x\"))]",
        "#[cfg(not(test))]",
        "#[cfg(feature = \"x\")]",
        "#[cfg_attr(test, inline)]",
        "#[cfg(unix)]",
    ] {
        let source = format!("{gate}\nfn f() {{}}");
        assert_eq!(context(&source, "f"), "", "{gate}");
    }
}

#[test]
fn a_function_nested_in_a_test_inherits_and_one_in_production_does_not() {
    let source =
        "#[test]\nfn t() { fn inner() {} inner(); }\nfn prod() { fn helper() {} helper(); }";
    let result = parse(source);
    assert_eq!(prop(&result, "inner", "code_context"), "test");
    assert_eq!(prop(&result, "helper", "code_context"), "");
}

#[test]
fn an_attribute_does_not_leak_to_the_next_item_and_lookalikes_do_not_count() {
    let source =
        "#[test]\nfn a() {}\nfn b() {}\n#[my::testing]\nfn c() {}\n#[contest]\nfn d() {}\n\
                  #[other::test]\nfn e() {}\n#[crate::test]\nfn f() {}";
    let result = parse(source);
    assert_eq!(prop(&result, "a", "code_context"), "test");
    for name in ["b", "c", "d", "e", "f"] {
        assert_eq!(prop(&result, name, "code_context"), "", "{name}");
    }
}
