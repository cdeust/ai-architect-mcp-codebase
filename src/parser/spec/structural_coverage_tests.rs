// parser::spec::structural_coverage_tests — the PR's primary deliverable
// (issue #224, owner pivot 2026-08-08): a language-by-language coverage table
// for the grammar-introspection engine (`structural.rs` + its TIER 2
// extensions in `structural_fallback.rs`), each row pinned by an assertion
// so it cannot silently drift, plus direct comparisons against the CURRENT
// production hand-written path (`crate::parser::parse_file`) on the same
// source, and one true zero-code-onboarding proof (Ruby).
//
// ## Method
//
// Every snippet below is a minimal, real, idiomatic definition + a call +
// (where the language has one) a heritage clause. Every row in the table was
// produced by actually running `parse_structural` and reading its output —
// none of this is predicted from reading grammar docs. This file is the
// SECOND generation of this table: the original (TIER 1 only) row for each
// of C, C++, ObjC, Swift, and kotlin-ng is preserved in this module's git
// history and in the PR description's before/after comparison; what follows
// is the AFTER state, produced by the TIER 2 mechanisms in
// `structural_fallback.rs` (declarator-chain descent, bounded identifier
// scan, heritage-on-child-node hop, kind-substring fallback classifier).
//
// ## Coverage table (10 hand-written languages + Ruby) — AFTER TIER 2
//
// | Language | Function/Method split | Struct/class shell | Calls | Inherits | Verdict |
// |---|---|---|---|---|---|
// | Python     | Yes | Yes | Yes | not tested this fixture | **Full recall** (unchanged, TIER 1 only) |
// | Java       | Yes | Yes | Yes | **Yes** (field-based) | **Full recall + heritage** (unchanged) |
// | TypeScript | Yes | Yes | Yes | **Yes** — NEW: heritage-on-child-node hop reads `class_heritage`'s nested `extends_clause` | **Full recall + heritage** (heritage gap CLOSED) |
// | Ruby       | Yes | Yes | Yes | not tested this fixture | **Full recall, zero new code** (unchanged) |
// | Rust       | Partial: top-level `fn` yes; `impl` block methods still **NO** — `impl_item`'s kind does not match the TIER 2 vocabulary (see "what remains unreachable" below) | Partial (only braced structs) | Yes | not tested | **Unchanged — Rust's `impl` idiom is the one target NOT in this PR's priority list and remains structurally unreached** |
// | C          | **YES — NEW**: declarator-chain descent finds both free functions | N/A (C has no class construct) | Yes | N/A | **Full recall** (definitions gap CLOSED) |
// | C++        | **YES — NEW**: declarator-chain descent finds the free function AND the class method, correctly scoped as a `HasMethod` edge | Yes | Yes, correctly attributed to the method (not the class) | **Yes — NEW**: heritage-on-child-node hop reads `base_class_clause` | **Full recall + heritage** (both gaps CLOSED) |
// | ObjC       | **YES — NEW**: bounded identifier-leaf descent finds the class name (a positional child preceding the `superclass` field); both interface-only method declarations AND implementation method definitions are indexed (two separate real declaration sites in the source, not a duplicate bug) | **YES — NEW** | Yes | **Yes** — the `superclass` field was ALREADY TIER-1-reachable once the class node itself gets a name (was previously unreachable transitively, since no def existed to hang the edge on) | **Full recall + heritage** (definitions gap CLOSED, which transitively closed heritage too) |
// | Swift      | **YES — NEW, split correctly**: `resolve_def_role` now consults `has_parameter_like_child`/`kind_hints_definition_role` even when TIER 1's `name`+`body` check already matched (Swift's grammar quirk — see the `resolve_def_role` doc) | Yes | **YES — NEW**: positional call fallback (with the `call_suffix`-wrapper false-positive fixed, see `structural_fallback`'s `kind_hints_positional_call`) | not tested this fixture | **Full recall** (both gaps CLOSED) |
// | Kotlin (kotlin-ng) | **YES — NEW** | **YES — NEW** | **YES — NEW** | N/A | **Total blackout → full recall** (every gap CLOSED) |
//
// ## What remains structurally unreachable (honest, not silently dropped)
//
// **Rust's `impl` block.** `impl_item`'s kind is `"impl_item"` — it does not
// contain `"function"`/`"method"`/`"class"`/`"struct"`/`"interface"`, nor
// does it end with a definition-site suffix in the sense
// `kind_hints_definition_role` checks (Rust names an impl by WHAT it
// implements, a fundamentally different grammar shape than "a definition
// with a name" that every other mechanism in this engine models). Reaching
// it would require a THIRD, Rust-specific vocabulary entry
// (`"impl_item"` itself, plus reading the `type` field as the enclosing
// scope's name instead of `name`) — which is no longer a small, reusable,
// cross-language rule; it is a per-construct special case for exactly one
// grammar's exactly one idiom. This PR does not add it, and documents that
// refusal here rather than silently leaving Rust's central OO idiom
// unexplained.
//
// ## Direct comparisons against the CURRENT hand-written path
//
// `structural_engine_reaches_java_deep_path_parity_on_this_shape` (Java,
// unaffected by this PR) and `structural_engine_now_matches_the_deep_path_on_c_definitions`
// (C — flipped from "misses every definition" to "matches" by TIER 2) run
// `crate::parser::parse_file` (the real production dispatch, unmodified by
// this PR — `src/parser/mod.rs`'s diff is empty) side by side with
// `parse_structural` on the SAME source, and assert the relationship
// directly — not inferred from the table above.

use super::structural::{parse_structural, StructuralSpec};
use crate::parser::{
    parse_file, Language, LABEL_CALL_SITE, LABEL_FUNCTION, LABEL_METHOD, LABEL_STRUCT,
};

fn labels_of(nodes: &[crate::parser::ExtractedNode], label: &str) -> Vec<String> {
    let mut v: Vec<String> = nodes
        .iter()
        .filter(|n| n.label == label)
        .map(|n| n.name.clone())
        .collect();
    v.sort();
    v
}

fn inherits_targets(refs: &[crate::parser::ExtractedRef]) -> Vec<String> {
    let mut v: Vec<String> = refs
        .iter()
        .filter(|e| e.kind == "Inherits")
        .map(|e| e.to_qualified_name.clone())
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------
// Unaffected by this PR: Python, Java, Ruby (TIER 1 only, still full recall).
// ---------------------------------------------------------------------------

#[test]
fn python_defs_calls_and_scoping_are_fully_recovered() {
    let spec = StructuralSpec {
        language: Language::Python,
        ts_language: || tree_sitter_python::LANGUAGE.into(),
    };
    let src = "def helper():\n    return 1\n\nclass Greeter:\n    def greet(self):\n        return helper()\n";
    let (r, stats) = parse_structural(&spec, src, "m.py").expect("python structural parse");
    assert_eq!(labels_of(&r.nodes, LABEL_FUNCTION), vec!["helper"]);
    assert_eq!(labels_of(&r.nodes, LABEL_STRUCT), vec!["Greeter"]);
    assert_eq!(labels_of(&r.nodes, LABEL_METHOD), vec!["greet"]);
    assert_eq!(labels_of(&r.nodes, LABEL_CALL_SITE), vec!["helper"]);
    assert!(
        r.refs
            .iter()
            .any(|e| e.kind == "HasMethod" && e.from_qualified_name.ends_with("Greeter")),
        "greet must be scoped to Greeter, got {:?}",
        r.refs.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
    assert_eq!(stats.unclassified_named_nodes_with_a_name_field, 0);
}

#[test]
fn java_defs_calls_and_inheritance_are_fully_recovered() {
    let spec = StructuralSpec {
        language: Language::Java,
        ts_language: || tree_sitter_java::LANGUAGE.into(),
    };
    let src = "class Greeter extends Base implements Nameable {\n    int helper() {\n        return 1;\n    }\n    String greet() {\n        return helper();\n    }\n}\n";
    let (r, _stats) = parse_structural(&spec, src, "Greeter.java").expect("java structural parse");
    assert_eq!(labels_of(&r.nodes, LABEL_STRUCT), vec!["Greeter"]);
    assert_eq!(labels_of(&r.nodes, LABEL_METHOD), vec!["greet", "helper"]);
    let inherits = inherits_targets(&r.refs);
    assert!(inherits.contains(&"Base".to_string()), "got {inherits:?}");
    assert!(
        inherits.contains(&"Nameable".to_string()),
        "got {inherits:?}"
    );
}

#[test]
fn java_visibility_is_recovered_via_the_modifiers_kind_child() {
    // TIER 2 item 4: `modifiers` is a child NODE kind, not a field, of
    // `method_declaration` (verified: the field list has no `modifiers`
    // entry despite the grammar defining that kind).
    let spec = StructuralSpec {
        language: Language::Java,
        ts_language: || tree_sitter_java::LANGUAGE.into(),
    };
    let src = "class Greeter {\n    public int greet() { return 1; }\n}\n";
    let (r, _stats) = parse_structural(&spec, src, "Greeter.java").expect("java structural parse");
    let greet = r
        .nodes
        .iter()
        .find(|n| n.label == LABEL_METHOD && n.name == "greet")
        .expect("greet method node");
    assert_eq!(greet.visibility, "public");
}

#[test]
fn ruby_reaches_full_recall_with_zero_new_code() {
    // The breadth proof: Ruby's grammar crate (`tree_sitter_ruby`) was
    // ALREADY a dependency before this PR (it backs the shallow path,
    // ADR-0056) — nothing was added to onboard it onto this engine beyond
    // this test itself constructing a `StructuralSpec` inline. No new Rust
    // module, no query file, no per-language table row anywhere in
    // `structural.rs`.
    let spec = StructuralSpec {
        language: Language::Ruby,
        ts_language: || tree_sitter_ruby::LANGUAGE.into(),
    };
    let src = "def helper()\n  1\nend\n\nclass Greeter\n  def greet()\n    helper()\n  end\nend\n";
    let (r, _stats) = parse_structural(&spec, src, "greeter.rb").expect("ruby structural parse");
    assert_eq!(labels_of(&r.nodes, LABEL_FUNCTION), vec!["helper"]);
    assert_eq!(labels_of(&r.nodes, LABEL_STRUCT), vec!["Greeter"]);
    assert_eq!(labels_of(&r.nodes, LABEL_METHOD), vec!["greet"]);
    assert_eq!(labels_of(&r.nodes, LABEL_CALL_SITE), vec!["helper"]);
    assert!(r
        .refs
        .iter()
        .any(|e| e.kind == "HasMethod" && e.from_qualified_name.ends_with("Greeter")));
}

#[test]
fn typescript_defs_calls_and_heritage_are_now_fully_recovered() {
    // Before TIER 2: defs+calls full, heritage NOT reachable (`class_heritage`
    // is a child NODE, not a field). After: `heritage_targets_via_child_hop`
    // closes this gap.
    let spec = StructuralSpec {
        language: Language::TypeScript,
        ts_language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    };
    let src = "function helper(): number {\n    return 1;\n}\n\nclass Greeter extends Base {\n    greet(): number {\n        return helper();\n    }\n}\n";
    let (r, _stats) = parse_structural(&spec, src, "m.ts").expect("ts structural parse");
    assert_eq!(labels_of(&r.nodes, LABEL_FUNCTION), vec!["helper"]);
    assert_eq!(labels_of(&r.nodes, LABEL_STRUCT), vec!["Greeter"]);
    assert_eq!(labels_of(&r.nodes, LABEL_METHOD), vec!["greet"]);
    assert_eq!(labels_of(&r.nodes, LABEL_CALL_SITE), vec!["helper"]);
    assert_eq!(inherits_targets(&r.refs), vec!["Base".to_string()]);
}

// ---------------------------------------------------------------------------
// Rust: unaffected by this PR (impl_item stays structurally unreached — see
// the module doc's "what remains unreachable" section).
// ---------------------------------------------------------------------------

#[test]
fn rust_finds_top_level_functions_but_not_impl_block_methods() {
    let spec = StructuralSpec {
        language: Language::Rust,
        ts_language: || tree_sitter_rust::LANGUAGE.into(),
    };
    let src = "fn helper() -> i32 {\n    1\n}\n\nstruct Greeter;\n\nimpl Greeter {\n    fn greet(&self) -> i32 {\n        helper()\n    }\n}\n";
    let (r, _stats) = parse_structural(&spec, src, "main.rs").expect("rust structural parse");
    assert_eq!(
        labels_of(&r.nodes, LABEL_FUNCTION),
        vec!["greet", "helper"],
        "impl_item's kind does not match the TIER 2 definition-site vocabulary \
         (see the module doc), so `greet` is still found only via the enclosing \
         `impl` block being invisible as a scope — it lands at file scope, \
         mislabeled Function, exactly as before this PR"
    );
    assert!(
        labels_of(&r.nodes, LABEL_STRUCT).is_empty(),
        "a unit struct (`struct Greeter;`) has no `body` field — genuinely \
         undetectable by the name+body rule, unchanged by this PR"
    );
}

// ---------------------------------------------------------------------------
// C, C++, ObjC: TIER 2 declarator-chain descent + bounded identifier scan
// close the definitions gap this PR targeted.
// ---------------------------------------------------------------------------

#[test]
fn c_now_reaches_full_recall_via_declarator_chain_descent() {
    let spec = StructuralSpec {
        language: Language::C,
        ts_language: || tree_sitter_c::LANGUAGE.into(),
    };
    let src = "int helper() {\n    return 1;\n}\n\nint greet() {\n    return helper();\n}\n";
    let (r, _stats) = parse_structural(&spec, src, "m.c").expect("c structural parse");
    assert_eq!(
        labels_of(&r.nodes, LABEL_FUNCTION),
        vec!["greet", "helper"],
        "declarator-chain descent (structural_fallback::resolve_name_via_declarator_chain) \
         now resolves C's nested identifier — the prior gap is closed"
    );
    assert_eq!(labels_of(&r.nodes, LABEL_CALL_SITE), vec!["helper"]);
}

#[test]
fn cpp_now_finds_the_method_and_its_heritage() {
    let spec = StructuralSpec {
        language: Language::Cpp,
        ts_language: || tree_sitter_cpp::LANGUAGE.into(),
    };
    let src = "int helper() {\n    return 1;\n}\n\nclass Greeter : public Base {\npublic:\n    int greet() {\n        return helper();\n    }\n};\n";
    let (r, _stats) = parse_structural(&spec, src, "m.cpp").expect("cpp structural parse");
    assert_eq!(labels_of(&r.nodes, LABEL_FUNCTION), vec!["helper"]);
    assert_eq!(labels_of(&r.nodes, LABEL_STRUCT), vec!["Greeter"]);
    assert_eq!(
        labels_of(&r.nodes, LABEL_METHOD),
        vec!["greet"],
        "the method, previously invisible, is now found AND correctly scoped \
         under Greeter (declarator-chain descent + resolve_scopes' existing \
         type-like-ancestor rule)"
    );
    assert!(
        r.refs
            .iter()
            .any(|e| e.kind == "HasMethod" && e.from_qualified_name.ends_with("Greeter")),
        "got {:?}",
        r.refs
            .iter()
            .map(|e| (&e.kind, &e.from_qualified_name))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        inherits_targets(&r.refs),
        vec!["Base".to_string()],
        "heritage-on-child-node hop (structural_fallback::heritage_targets_via_child_hop) \
         reads C++'s base_class_clause, a child NODE not a field"
    );
}

#[test]
fn objc_now_finds_the_class_its_methods_and_its_superclass() {
    let spec = StructuralSpec {
        language: Language::ObjC,
        ts_language: || tree_sitter_objc::LANGUAGE.into(),
    };
    let src = "@interface Greeter : NSObject\n- (int)greet;\n@end\n\n@implementation Greeter\n- (int)greet {\n    return [self helper];\n}\n- (int)helper {\n    return 1;\n}\n@end\n";
    let (r, _stats) = parse_structural(&spec, src, "Greeter.m").expect("objc structural parse");
    assert_eq!(
        labels_of(&r.nodes, LABEL_STRUCT),
        vec!["Greeter", "Greeter"],
        "TWO Greeter def sites: the @interface declaration and the \
         @implementation definition are two separate, real AST nodes in \
         ObjC's grammar — this is the language's own duplication, not an \
         engine bug (see resolve_scopes' @{{line}} dedup suffix in the \
         qualified names, exercised below)"
    );
    let qns: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.label == LABEL_STRUCT)
        .map(|n| n.qualified_name.as_str())
        .collect();
    assert!(qns.contains(&"Greeter.m::Greeter"));
    assert!(qns.iter().any(|qn| qn.starts_with("Greeter.m::Greeter@")));
    assert_eq!(
        labels_of(&r.nodes, LABEL_METHOD),
        vec!["greet", "greet", "helper"],
        "the @interface's bodyless method signature (`- (int)greet;`) AND \
         both @implementation methods are all real declaration sites"
    );
    assert_eq!(labels_of(&r.nodes, LABEL_CALL_SITE), vec!["helper"]);
    assert_eq!(
        inherits_targets(&r.refs),
        vec!["NSObject".to_string()],
        "the `superclass` field was already TIER-1-readable — it was \
         unreachable only because no def previously existed to hang the \
         edge on; fixing the definition transitively fixed the heritage \
         edge too, and it now correctly attaches to the interface's \
         Greeter (verified: NOT the @implementation's Greeter@N)"
    );
}

// ---------------------------------------------------------------------------
// Swift, Kotlin (kotlin-ng): TIER 2 kind-substring fallback classifier +
// positional call fallback close the fieldless-grammar gap this PR targeted
// — the hardest case, per the task's own framing.
// ---------------------------------------------------------------------------

#[test]
fn swift_now_splits_function_from_method_from_class_and_finds_calls() {
    let spec = StructuralSpec {
        language: Language::Swift,
        ts_language: || tree_sitter_swift::LANGUAGE.into(),
    };
    let src = "func helper() -> Int {\n    return 1\n}\n\nclass Greeter {\n    func greet() -> Int {\n        return helper()\n    }\n}\n";
    let (r, _stats) =
        parse_structural(&spec, src, "Greeter.swift").expect("swift structural parse");
    assert_eq!(
        labels_of(&r.nodes, LABEL_FUNCTION),
        vec!["helper"],
        "resolve_def_role now consults the kind-substring vocabulary even \
         when TIER 1's name+body check already matched (Swift's \
         function_declaration has both fields), closing the \
         Function-vs-Type ambiguity"
    );
    assert_eq!(labels_of(&r.nodes, LABEL_STRUCT), vec!["Greeter"]);
    assert_eq!(labels_of(&r.nodes, LABEL_METHOD), vec!["greet"]);
    assert_eq!(
        labels_of(&r.nodes, LABEL_CALL_SITE),
        vec!["helper"],
        "positional call fallback finds call_expression's callee via its \
         first named child, with the call_suffix false-positive excluded \
         (see structural_fallback::kind_hints_positional_call)"
    );
}

#[test]
fn kotlin_ng_now_reaches_full_recall() {
    let spec = StructuralSpec {
        language: Language::Kotlin,
        ts_language: || tree_sitter_kotlin_ng::LANGUAGE.into(),
    };
    let src = "fun helper(): Int {\n    return 1\n}\n\nclass Greeter {\n    fun greet(): Int {\n        return helper()\n    }\n}\n";
    let (r, stats) = parse_structural(&spec, src, "Greeter.kt").expect("kotlin structural parse");
    assert_eq!(labels_of(&r.nodes, LABEL_FUNCTION), vec!["helper"]);
    assert_eq!(labels_of(&r.nodes, LABEL_STRUCT), vec!["Greeter"]);
    assert_eq!(labels_of(&r.nodes, LABEL_METHOD), vec!["greet"]);
    assert_eq!(labels_of(&r.nodes, LABEL_CALL_SITE), vec!["helper"]);
    assert!(
        r.refs
            .iter()
            .any(|e| e.kind == "HasMethod" && e.from_qualified_name.ends_with("Greeter")),
        "got {:?}",
        r.refs
            .iter()
            .map(|e| (&e.kind, &e.from_qualified_name))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        stats.unclassified_named_nodes_with_a_name_field, 0,
        "kotlin-ng went from a total-blackout honesty counter to zero \
         unclassified nodes on this shape"
    );
}

// ---------------------------------------------------------------------------
// Direct comparisons against the CURRENT production hand-written path
// (`crate::parser::parse_file`, unmodified by this PR — src/parser/mod.rs's
// diff is empty).
// ---------------------------------------------------------------------------

#[test]
fn structural_engine_reaches_java_deep_path_parity_on_this_shape() {
    let src = "class Greeter extends Base implements Nameable {\n    int helper() {\n        return 1;\n    }\n    String greet() {\n        return helper();\n    }\n}\n";
    let deep = parse_file(src, "Greeter.java", Language::Java).expect("deep java parse");
    let spec = StructuralSpec {
        language: Language::Java,
        ts_language: || tree_sitter_java::LANGUAGE.into(),
    };
    let (structural, _stats) =
        parse_structural(&spec, src, "Greeter.java").expect("structural java parse");
    assert_eq!(
        labels_of(&deep.nodes, LABEL_METHOD),
        labels_of(&structural.nodes, LABEL_METHOD),
        "on this shape the structural engine's Method set equals the hand-written deep path's"
    );
    assert_eq!(
        labels_of(&deep.nodes, LABEL_STRUCT),
        labels_of(&structural.nodes, LABEL_STRUCT)
    );
}

#[test]
fn structural_engine_now_matches_the_deep_path_on_c_definitions() {
    // Flipped from `structural_engine_misses_every_c_definition_the_deep_path_finds`
    // (the PRE-TIER-2 pinned regression) now that declarator-chain descent
    // closes the gap.
    let src = "int helper() {\n    return 1;\n}\n\nint greet() {\n    return helper();\n}\n";
    let deep = parse_file(src, "m.c", Language::C).expect("deep c parse");
    let spec = StructuralSpec {
        language: Language::C,
        ts_language: || tree_sitter_c::LANGUAGE.into(),
    };
    let (structural, _stats) = parse_structural(&spec, src, "m.c").expect("structural c parse");
    assert_eq!(
        labels_of(&deep.nodes, LABEL_FUNCTION),
        labels_of(&structural.nodes, LABEL_FUNCTION),
        "the structural engine's Function set now equals the hand-written \
         deep path's on this shape — quantified directly against production \
         output, not inferred"
    );
}
