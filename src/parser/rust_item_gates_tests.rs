use super::*;

fn gate_at(source: &str, line: u64) -> FunctionGate {
    function_gates(source)
        .expect("parses")
        .into_iter()
        .find(|f| f.line == line)
        .unwrap_or_else(|| panic!("no function at line {line}"))
}

#[test]
fn an_ungated_function_has_the_empty_gate() {
    let f = gate_at("#[test]\nfn t() {}\n", 2);
    assert_eq!(f.gate, "");
    assert!(!f.cfg_attr && !f.nested);
}

#[test]
fn a_test_module_gates_its_functions_with_test_only() {
    let source = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n";
    assert_eq!(gate_at(source, 4).gate, "test");
}

#[test]
fn a_feature_gate_on_the_module_reaches_the_function() {
    let source = "#[cfg(feature = \"x\")]\nmod m {\n    #[test]\n    fn t() {}\n}\n";
    assert_ne!(gate_at(source, 4).gate, "");
    assert_ne!(gate_at(source, 4).gate, "test");
}

#[test]
fn a_cfg_attr_anywhere_above_marks_the_gate_unknown() {
    let source = "#[cfg_attr(feature = \"x\", cfg(test))]\nmod m {\n    fn t() {}\n}\n";
    assert!(gate_at(source, 3).cfg_attr);
    let inner = "#![cfg_attr(docsrs, cfg(test))]\nfn t() {}\n";
    assert!(gate_at(inner, 2).cfg_attr);
}

#[test]
fn a_cfg_attr_that_adds_no_cfg_gates_nothing() {
    let source = "#![cfg_attr(not(test), no_std)]\n#![forbid(unsafe_code)]\n#[cfg_attr(test, derive(Debug))]\nfn t() {}\n";
    assert!(!gate_at(source, 4).cfg_attr);
    let nested = "#[cfg_attr(a, cfg_attr(b, cfg(c)))]\nfn t() {}\n";
    assert!(gate_at(nested, 2).cfg_attr);
    let quoted = "#[cfg_attr(feature = \"a,cfg(x)\", doc = \"cfg(y)\")]\nfn t() {}\n";
    assert!(!gate_at(quoted, 2).cfg_attr);
}

#[test]
fn a_function_inside_a_function_is_nested() {
    let source = "fn outer() {\n    #[test]\n    fn inner() {}\n}\n";
    assert!(gate_at(source, 3).nested);
    assert!(!gate_at(source, 1).nested);
}

#[test]
fn enum_shapes_count_variants_and_those_with_their_own_cfg() {
    let source = "enum E {\n    A,\n    #[cfg(unix)]\n    B(u8),\n    C { x: u8 },\n    #[cfg_attr(x, cfg(y))]\n    D,\n    #[cfg_attr(x, doc = \"d\")]\n    F,\n}\n";
    let shapes = enum_shapes(source).expect("parses");
    assert_eq!(
        shapes,
        vec![EnumShape {
            line: 1,
            variants: 5,
            gated_variants: 2
        }]
    );
}

#[test]
fn an_oversized_source_is_refused_before_parsing() {
    let big = "a".repeat(MAX_PARSE_BYTES as usize + 1);
    assert!(function_gates(&big).is_err());
}
