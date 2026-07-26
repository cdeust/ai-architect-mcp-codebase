// parser::spec::shallow_tests — behaviour pins for the ADR-0056 shallow path.
//
// Two things are pinned here, and they are different in kind:
//   1. What the shallow path DOES emit (definitions, methods scoped to their
//      class, calls, imports) — ordinary positive assertions.
//   2. What it deliberately does NOT emit (visibility, inheritance) — negative
//      assertions (§13.1 G4). Absence IS the contract: ADR-0056 decision 3
//      chose an empty field over a guessed one because a name-case guess is
//      wrong for modifier-keyword languages, and a false property in the graph
//      is worse than a missing one. Without these, a later "helpful" default
//      would silently reintroduce the defect.

use super::registry::SHALLOW_SPECS;
use super::ruby::RUBY_SPEC;
use super::shallow::{parse_shallow, ShallowSpec};
use crate::parser::{
    ExtractedNode, Language, ParseResult, LABEL_CALL_SITE, LABEL_FUNCTION, LABEL_IMPORT,
    LABEL_METHOD, LABEL_STRUCT,
};

const RUBY_FIXTURE: &str = r#"
require 'json'

module Billing
  class Invoice
    def initialize(total)
      @total = total
    end

    def render
      formatter.format(@total)
    end

    def self.build(rows)
      Invoice.new(sum(rows))
    end
  end
end

def standalone
  puts "hi"
end
"#;

fn ruby() -> ParseResult {
    parse_shallow(&RUBY_SPEC, RUBY_FIXTURE, "billing.rb").expect("ruby shallow parse")
}

fn names_with_label<'a>(r: &'a ParseResult, label: &str) -> Vec<&'a str> {
    let mut v: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.label == label)
        .map(|n| n.name.as_str())
        .collect();
    v.sort_unstable();
    v
}

fn edges_of<'a>(r: &'a ParseResult, kind: &str) -> Vec<(&'a str, &'a str)> {
    let mut v: Vec<(&str, &str)> = r
        .refs
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| (e.from_qualified_name.as_str(), e.to_qualified_name.as_str()))
        .collect();
    v.sort_unstable();
    v
}

#[test]
fn ruby_shallow_extracts_classes_methods_and_functions() {
    let r = ruby();
    // `module Billing` and `class Invoice` both map to Struct: a Ruby module is
    // a namespace whose methods must scope to it.
    assert_eq!(
        names_with_label(&r, LABEL_STRUCT),
        vec!["Billing", "Invoice"]
    );
    // `def self.build` is a `singleton_method` => Method regardless of nesting;
    // the instance methods are Methods by enclosing class.
    let methods = names_with_label(&r, LABEL_METHOD);
    assert!(
        methods.contains(&"build"),
        "singleton_method must be a Method, got {methods:?}"
    );
    assert!(
        methods.contains(&"render") && methods.contains(&"initialize"),
        "in-class defs must be Methods, got {methods:?}"
    );
    // A top-level `def` is a Function, not a Method.
    assert_eq!(names_with_label(&r, LABEL_FUNCTION), vec!["standalone"]);
}

#[test]
fn ruby_methods_are_scoped_to_their_enclosing_class() {
    let r = ruby();
    let has_method = edges_of(&r, "HasMethod");
    assert!(
        has_method
            .iter()
            .any(|(from, to)| from.ends_with("Billing::Invoice") && to.contains("::render")),
        "render must hang off Billing::Invoice, got {has_method:?}"
    );
    // The module nests the class: Defines chains file -> Billing -> Invoice.
    let defines = edges_of(&r, "Defines");
    assert!(
        defines
            .iter()
            .any(|(from, to)| *from == "billing.rb" && to.ends_with("::Billing")),
        "file must define the module, got {defines:?}"
    );
    assert!(
        defines
            .iter()
            .any(|(from, to)| from.ends_with("::Billing") && to.ends_with("Billing::Invoice")),
        "module must define the class, got {defines:?}"
    );
}

#[test]
fn ruby_call_callee_is_the_method_not_the_receiver() {
    // The `callee_field: Some("method")` pin. tree-sitter-ruby models
    // `formatter.format(...)` as a `call` with fields receiver=formatter and
    // method=format, so the call's FIRST NAMED CHILD is the receiver. Dropping
    // `callee_field` would record `formatter` here — this test kills that
    // mutant, and it is the reason the field exists rather than a walker
    // conditional (ADR-0056 Risk #1).
    let r = ruby();
    let callees = names_with_label(&r, LABEL_CALL_SITE);
    assert!(
        callees.contains(&"format"),
        "callee must be the method `format`, got {callees:?}"
    );
    assert!(
        !callees.contains(&"formatter"),
        "callee must NOT be the receiver `formatter`, got {callees:?}"
    );
    // Calls edges point at the callee name.
    assert!(
        edges_of(&r, "Calls").iter().any(|(_, to)| *to == "format"),
        "a Calls edge must target `format`"
    );
}

#[test]
fn ruby_require_surfaces_as_a_call_not_a_synthesised_import() {
    // Ruby has no import statement node; `require 'json'` is an ordinary call.
    // ADR-0056 chooses to report that honestly rather than synthesise an
    // Import, so the graph never claims an import edge the grammar cannot
    // support.
    let r = ruby();
    assert!(
        names_with_label(&r, LABEL_CALL_SITE).contains(&"require"),
        "require must be visible as a call"
    );
    assert!(
        r.nodes.iter().all(|n| n.label != LABEL_IMPORT),
        "Ruby must emit no Import nodes (import_node_kinds is empty)"
    );
}

#[test]
fn shallow_nodes_carry_no_visibility_and_no_inheritance() {
    // ADR-0056 decision 3, asserted as absence. A future "reasonable default"
    // (uppercase => public) would be WRONG for Java/Swift/Kotlin/C# and is
    // exactly what this pin exists to block.
    let r = ruby();
    let with_visibility: Vec<&ExtractedNode> = r
        .nodes
        .iter()
        .filter(|n| !n.visibility.is_empty())
        .collect();
    assert!(
        with_visibility.is_empty(),
        "shallow nodes must carry NO visibility; offenders: {:?}",
        with_visibility
            .iter()
            .map(|n| (&n.name, &n.visibility))
            .collect::<Vec<_>>()
    );
    for forbidden in ["Extends", "Implements", "HasVariant", "HasField"] {
        assert!(
            r.refs.iter().all(|e| e.kind != forbidden),
            "shallow path must emit no {forbidden} edges"
        );
    }
}

#[test]
fn shallow_dedups_same_named_definitions_in_one_file() {
    // Two `def run` in one file must not collide on one primary key.
    let src = "class A\n  def run\n  end\nend\nclass B\n  def run\n  end\nend\n";
    let r = parse_shallow(&RUBY_SPEC, src, "d.rb").expect("parse");
    let qns: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.label == LABEL_METHOD)
        .map(|n| n.qualified_name.as_str())
        .collect();
    assert_eq!(qns.len(), 2, "both methods must be emitted, got {qns:?}");
    assert_ne!(qns[0], qns[1], "same-named methods need distinct QNs");
}

#[test]
fn shallow_parse_reports_errors_rather_than_dropping_them() {
    // A malformed file still returns Ok with a non-zero parse_errors signal,
    // so a mis-detected language is observable rather than silently empty.
    let r = parse_shallow(&RUBY_SPEC, "class Invoice\n  def broken(\n", "b.rb").expect("parse");
    assert!(
        r.parse_errors > 0,
        "malformed Ruby must report parse_errors, got {}",
        r.parse_errors
    );
}

#[test]
fn shallow_handles_an_empty_file_without_emitting_anything() {
    let r = parse_shallow(&RUBY_SPEC, "", "e.rb").expect("parse");
    assert!(r.nodes.is_empty() && r.refs.is_empty());
}

// ---------------------------------------------------------------------------
// The import path, exercised on a grammar that HAS import statements.
//
// Ruby has none, so a test-only Python shallow row proves the generic
// keyword-stripping rule: tree-sitter marks keywords (`import`, `from`) as
// UNNAMED children, so reading only named children removes them with no
// per-language keyword list. That is the device that keeps the walker free of
// per-language conditionals, and it needs its own pin.
// ---------------------------------------------------------------------------

static PYTHON_SHALLOW: ShallowSpec = ShallowSpec {
    language: Language::Python,
    function_node_kinds: &["function_definition"],
    method_node_kinds: &[],
    class_node_kinds: &["class_definition"],
    call_node_kinds: &["call"],
    import_node_kinds: &["import_statement", "import_from_statement"],
    callee_field: Some("function"),
    name_field: "name",
    body_field: Some("body"),
    ts_language: || tree_sitter_python::LANGUAGE.into(),
};

#[test]
fn shallow_imports_strip_keywords_via_named_children() {
    let src = "import os\nfrom pkg.mod import thing\n";
    let r = parse_shallow(&PYTHON_SHALLOW, src, "m.py").expect("parse");
    let paths: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.label == LABEL_IMPORT)
        .map(|n| {
            n.properties
                .iter()
                .find(|(k, _)| k == "path")
                .map(|(_, v)| v.as_str())
                .unwrap_or("")
        })
        .collect();
    assert_eq!(paths.len(), 2, "two imports expected, got {paths:?}");
    for p in &paths {
        assert!(
            !p.contains("import") && !p.contains("from"),
            "keyword leaked into import path {p:?} — named-children stripping failed"
        );
    }
    assert!(paths.contains(&"os"), "got {paths:?}");
    assert!(
        paths.iter().any(|p| p.contains("pkg.mod")),
        "from-import must keep its module path, got {paths:?}"
    );
}

// ---------------------------------------------------------------------------
// ADR-0056 Risk #1 tripwire
// ---------------------------------------------------------------------------

#[test]
fn shallow_walker_has_no_language_conditionals() {
    // The reference implementation's defect is 176 `lang == CBM_LANG_*` sites
    // across its extractors — the growing-conditional-chain §1.2 forbids.
    // ADR-0055's Risk #1 fired five times before being acted on; this one is
    // machine-checked from the first commit.
    //
    // A language whose grammar the shallow fields cannot express is promoted to
    // the deep path or left unsupported. It does NOT earn a conditional here.
    let src = include_str!("shallow.rs");
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let offenders: Vec<&str> = code
        .lines()
        .filter(|l| l.contains("Language::"))
        .map(|l| l.trim())
        .collect();
    assert!(
        offenders.is_empty(),
        "the shallow walker must contain no per-language conditional; found:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn shallow_tripwire_would_catch_a_real_conditional() {
    // Proves the tripwire is not vacuous: the same detection applied to text
    // that DOES contain a language conditional must fire. Without this, a
    // future refactor that broke the scan would leave a silently-passing guard
    // (the vacuous-static-check failure this repo has hit before).
    let planted = "fn f() {\n    if spec.language == Language::Ruby { todo!() }\n}";
    let found = planted
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .any(|l| l.contains("Language::"));
    assert!(
        found,
        "the tripwire's detection must fire on a real conditional"
    );
}

#[test]
fn every_shallow_language_is_reachable_through_parse_file() {
    // A spec row nobody dispatches to is dead data (§9). Each shallow language
    // must round-trip: its tag parses back, and `parse_file` — the real entry
    // point, not `parse_shallow` directly — routes it to a real extraction.
    for spec in SHALLOW_SPECS {
        let tag = spec.language.as_str();
        assert_eq!(
            Language::from_str_opt(tag),
            Some(spec.language),
            "language tag {tag:?} must parse back to its variant"
        );
    }
    let r = crate::parser::parse_file(RUBY_FIXTURE, "billing.rb", Language::Ruby)
        .expect("parse_file must route Ruby to the shallow path");
    assert!(
        !r.nodes.is_empty(),
        "parse_file must produce Ruby nodes; an unrouted language yields nothing"
    );
}

#[test]
fn ruby_files_are_detected_and_resolvable_end_to_end() {
    // The wiring that is easy to forget and silently useless if missed: the
    // extension must map to the language, and `ALL_EXTENSIONS` must know it or
    // `extract_file_prefix` cannot recover the originating file from a Ruby
    // node id, breaking resolution for every Ruby symbol.
    assert_eq!(Language::from_extension("rb"), Some(Language::Ruby));
    assert!(
        crate::language_provider::ALL_EXTENSIONS.contains(&"rb"),
        "ALL_EXTENSIONS must include rb, or Ruby node ids resolve to no file"
    );
    assert_eq!(
        crate::language_provider::extract_file_prefix("billing.rb::Billing::Invoice"),
        Some("billing.rb".to_string()),
        "a Ruby node id must resolve back to its file"
    );
}
