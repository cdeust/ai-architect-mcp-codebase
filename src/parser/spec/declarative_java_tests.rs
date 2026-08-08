// parser::spec::declarative_java_tests — direct unit tests for
// `VisibilityRule::ModifierKeyword { modifier_source: ModifierSource::ChildKind
// }`, exercised against REAL tree-sitter-java output (not a stand-in
// grammar), split out of `declarative_tests.rs` to keep that file under the
// coding-standards §4.1 500-line cap (a pure move for the split; the tests
// themselves are new in issue #220 phase 2).
//
// tree-sitter-java 0.23.5's `node-types.json` lists `modifiers` under every
// declaration's `children`, never under `fields` (verified directly, not
// assumed — see `ModifierSource`'s doc comment in `declarative_rules.rs`), so
// `ChildKind` is the shape Java's own `JAVA_RULES` (`java.rs`) selects in
// production. These tests prove the interpreter branch directly, independent
// of `java_declarative_parity_tests.rs`'s end-to-end corpus parity check.

use super::conventions::LanguageConventions;
use super::declarative::DeclarativeConventions;
use super::declarative_rules::{
    CallEntryRule, CallSiteQnScheme, ConventionSpec, ImportRule, ModifierSource, PropertySet,
    QnScheme, RefToRule, VisibilityRule,
};
use super::declarative_tests::first_node_of_kind;

static MODIFIER_KEYWORD_CHILD_KIND_SPEC: ConventionSpec = ConventionSpec {
    visibility: VisibilityRule::ModifierKeyword {
        modifier_source: ModifierSource::ChildKind("modifiers"),
        candidates: &[
            ("public", "public"),
            ("private", "private"),
            ("protected", "protected"),
        ],
        default_label: "package",
    },
    receiver_pattern: None,
    qn_scheme: QnScheme::SeqSuffixed,
    callee_dispatch: &[],
    call_entry: CallEntryRule {
        qn_scheme: CallSiteQnScheme::LineColSeq,
        ref_kind: "Calls",
        ref_to: RefToRule::Verbatim,
        visibility: "package",
        properties: PropertySet::CalleeOnly,
    },
    import_rule: ImportRule::StatementStrip {
        strip_prefixes: &["import", "static"],
        trim_end: &[';'],
        delimiter_pair: None,
    },
};

fn parse_java(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

#[test]
fn modifier_keyword_node_visibility_scans_a_child_kind() {
    let conv = DeclarativeConventions(&MODIFIER_KEYWORD_CHILD_KIND_SPEC);
    let source = "public class Foo {}\n";
    let tree = parse_java(source);
    let class = first_node_of_kind(&tree, source, "class_declaration");
    assert_eq!(conv.node_visibility(source, class, "Foo"), "public");

    let source_default = "class Bar {}\n";
    let tree_default = parse_java(source_default);
    let class_default = first_node_of_kind(&tree_default, source_default, "class_declaration");
    assert_eq!(
        conv.node_visibility(source_default, class_default, "Bar"),
        "package",
        "no modifiers child at all -> the rule's default_label"
    );
}

#[test]
fn modifier_keyword_node_visibility_does_not_leak_a_nested_declarations_modifier() {
    // `Outer` has NO modifier of its own; `Inner` does. A whole-subtree or
    // whole-node-text scan (phase 1's original `None` fallback) would find
    // "public" from `Inner`'s modifiers while computing `Outer`'s visibility
    // — this is exactly the shared-layer defect issue #220 phase 2 fixes.
    let conv = DeclarativeConventions(&MODIFIER_KEYWORD_CHILD_KIND_SPEC);
    let source = "class Outer { public class Inner {} }\n";
    let tree = parse_java(source);
    let outer = first_node_of_kind(&tree, source, "class_declaration");
    assert_eq!(
        conv.node_visibility(source, outer, "Outer"),
        "package",
        "Outer has no modifiers child of its own; Inner's must not leak in"
    );
}
