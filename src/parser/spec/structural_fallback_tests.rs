// parser::spec::structural_fallback_tests — split out purely for the
// coding-standards.md §4.1 500-line file cap once the fieldless-call
// fallback (issue #224's Elixir/Zig/Bash follow-up) landed and pushed
// structural_fallback.rs over the limit (mirrors why structural_imports_tests
// was split from structural_imports.rs) — no new mechanism lives here, only
// the per-grammar verification tests for that module's public(-to-`spec`)
// surface.

use super::structural_fallback::*;

fn parse(lang: tree_sitter::Language, src: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).unwrap();
    parser.parse(src, None).unwrap()
}

#[test]
fn declarator_chain_finds_c_function_name() {
    let src = "int helper() {\n    return 1;\n}\n";
    let tree = parse(tree_sitter_c::LANGUAGE.into(), src);
    let func = tree.root_node().named_child(0).unwrap();
    assert_eq!(func.kind(), "function_definition");
    let name = resolve_name_via_declarator_chain(func).expect("name found");
    assert_eq!(&src[name.byte_range()], "helper");
}

#[test]
fn declarator_chain_finds_cpp_method_name() {
    let src = "class Greeter {\npublic:\n    int greet() { return 1; }\n};\n";
    let tree = parse(tree_sitter_cpp::LANGUAGE.into(), src);
    let class_node = tree.root_node().named_child(0).unwrap();
    let body = class_node.child_by_field_name("body").unwrap();
    let mut cursor = body.walk();
    let method = body
        .named_children(&mut cursor)
        .find(|n| n.kind() == "function_definition")
        .expect("method node");
    let name = resolve_name_via_declarator_chain(method).expect("name found");
    assert_eq!(&src[name.byte_range()], "greet");
}

#[test]
fn bounded_scan_finds_objc_class_name_before_superclass() {
    let src = "@interface Greeter : NSObject\n@end\n";
    let tree = parse(tree_sitter_objc::LANGUAGE.into(), src);
    let iface = tree.root_node().named_child(0).unwrap();
    let name = resolve_name_via_bounded_scan(iface).expect("name found");
    assert_eq!(&src[name.byte_range()], "Greeter");
}

#[test]
fn descent_does_not_confuse_a_class_return_type_with_the_function_name() {
    // A C++ method returning a class type: the `type` field is
    // `type_identifier`, which must NOT be picked up by the
    // EXACT-kind `"identifier"` bounded scan even if reached first in
    // document order. The declarator-chain tier is what actually
    // resolves this case in production (this node has a `declarator`
    // field, so `resolve_name_via_bounded_scan` is never even reached
    // for it) — this test pins the bounded-scan tier's kind exactness
    // as an independent safety property.
    let src = "class Greeter {\npublic:\n    Greeter make() { return Greeter(); }\n};\n";
    let tree = parse(tree_sitter_cpp::LANGUAGE.into(), src);
    let class_node = tree.root_node().named_child(0).unwrap();
    let body = class_node.child_by_field_name("body").unwrap();
    let mut cursor = body.walk();
    let method = body
        .named_children(&mut cursor)
        .find(|n| n.kind() == "function_definition")
        .expect("method node");
    // The declarator-chain tier (the one actually used in production)
    // must return "make", not the "Greeter" return type.
    let via_chain = resolve_name_via_declarator_chain(method).expect("name found");
    assert_eq!(&src[via_chain.byte_range()], "make");
    // And the bounded-scan tier, if it were run standalone on this same
    // node, must not match the return-type's `type_identifier` either
    // (kind mismatch by design) -- it would instead find "make" via
    // `field_identifier`... which ALSO does not match exact
    // `"identifier"`, so it correctly finds nothing, proving no false
    // positive from the return type.
    assert!(resolve_name_via_bounded_scan(method).is_none());
}

#[test]
fn kind_hints_recognize_swift_and_kotlin_shared_kind_names() {
    assert_eq!(
        kind_hints_definition_role("function_declaration"),
        Some(KindRole::FunctionLike)
    );
    assert_eq!(
        kind_hints_definition_role("class_declaration"),
        Some(KindRole::TypeLike)
    );
    assert_eq!(kind_hints_definition_role("return_statement"), None);
    assert_eq!(
        kind_hints_definition_role("class_interface"),
        Some(KindRole::TypeLike)
    );
    assert_eq!(
        kind_hints_definition_role("class_implementation"),
        Some(KindRole::TypeLike)
    );
    assert_eq!(
        kind_hints_definition_role("function_definition"),
        Some(KindRole::FunctionLike)
    );
}

#[test]
fn kind_hints_reject_wrapper_kinds_that_merely_contain_the_same_keyword() {
    // Verified false-positive risk: kotlin-ng's parameter-list wrapper
    // literally contains "function"; TypeScript/kotlin-ng's body/
    // heritage wrappers literally contain "class". None end with a
    // definition-site suffix, so the joint (substring AND suffix) rule
    // must reject all three.
    assert_eq!(
        kind_hints_definition_role("function_value_parameters"),
        None
    );
    assert_eq!(kind_hints_definition_role("class_body"), None);
    assert_eq!(kind_hints_definition_role("class_heritage"), None);
}

#[test]
fn heritage_child_hop_finds_typescript_extends_target() {
    let src = "class Greeter extends Base {\n  greet(): number { return 1; }\n}\n";
    let tree = parse(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), src);
    let class_node = tree.root_node().named_child(0).unwrap();
    let targets = heritage_targets_via_child_hop(class_node);
    let texts: Vec<&str> = targets.iter().map(|n| &src[n.byte_range()]).collect();
    assert_eq!(texts, vec!["Base"]);
}

#[test]
fn heritage_child_hop_finds_cpp_base_class_target() {
    let src = "class Greeter : public Base {\npublic:\n    int greet() { return 1; }\n};\n";
    let tree = parse(tree_sitter_cpp::LANGUAGE.into(), src);
    let class_node = tree.root_node().named_child(0).unwrap();
    let targets = heritage_targets_via_child_hop(class_node);
    let texts: Vec<&str> = targets.iter().map(|n| &src[n.byte_range()]).collect();
    assert_eq!(texts, vec!["Base"]);
}

#[test]
fn fieldless_call_fallback_finds_zigs_builtin_import() {
    let src = "const std = @import(\"std\");\n";
    let tree = parse(tree_sitter_zig::LANGUAGE.into(), src);
    let decl = tree.root_node().named_child(0).unwrap();
    let mut cursor = decl.walk();
    let builtin = decl
        .named_children(&mut cursor)
        .find(|n| n.kind() == "builtin_function")
        .expect("builtin_function node");
    assert!(is_fieldless_call_with_positional_arguments(builtin));
    let callee = first_named_child(builtin).expect("callee");
    assert_eq!(&src[callee.byte_range()], "@import");
}

#[test]
fn fieldless_call_fallback_rejects_a_node_with_a_field_tagged_child() {
    // Elixir's `call` node has a `target` FIELD alongside its positional
    // `arguments` child — it must be excluded from this fallback (it is
    // reached instead via `call_callee_field`'s own `target` branch in
    // `structural.rs`, verified there, not here).
    let src = "import Enum\n";
    let tree = parse(tree_sitter_elixir::LANGUAGE.into(), src);
    let call = tree.root_node().named_child(0).unwrap();
    assert_eq!(call.kind(), "call");
    assert!(!is_fieldless_call_with_positional_arguments(call));
}

#[test]
fn positional_call_gate_rejects_swifts_call_suffix_wrapper() {
    // Verified false-positive risk: Swift's call_expression wraps its
    // argument list in a child `call_suffix` node, which ALSO contains
    // the substring "call". Without the "expression" suffix condition
    // this would double-fire per call site.
    assert!(kind_hints_positional_call("call_expression"));
    assert!(!kind_hints_positional_call("call_suffix"));
}
