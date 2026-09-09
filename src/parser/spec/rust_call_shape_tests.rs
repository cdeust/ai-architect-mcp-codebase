// parser::spec::rust_call_shape_tests — the two call-shape defects measured on
// DYResearch/dy-wcet @ 1e93ccd on 2026-09-09, each pinned by the shape that
// exposed it rather than by the corpus it was found in.
//
// Both were found by attempting a proof-coverage measurement (which production
// functions no Kani harness reaches) and discovering the answer was wrong in
// both directions at once:
//
//   A. `fn task(c, t, d, b, j) { Task::new(c, t)... }` emitted a CallSite for
//      EVERY bare argument identifier. `t` collided with a real `tests::t`
//      helper elsewhere in the corpus, so it resolved, and `get_impact` then
//      credited `tests::t` with 20 callers that do not call it.
//   B. `assert!(!Response::Unbounded(Unbounded::NonConvergent).meets(d))`
//      emitted NO CallSite for `meets`. Eight such calls existed in the
//      sources; the graph held zero. `get_impact(Response::meets)` answered
//      `epistemic: exact, callers: 0`, which is a confidently wrong answer
//      about a method four proof harnesses depend on.
//
// A is a false positive, B a false negative, and B is the more dangerous of
// the two because nothing downstream could tell it apart from a real absence.

use crate::parser::{parse_file, Language};

/// Every `CallSite` name the walker emits for `source`.
fn call_sites(source: &str) -> Vec<String> {
    let result = parse_file(source, "src/probe.rs", Language::Rust).expect("parse");
    result
        .nodes
        .iter()
        .filter(|n| n.label == "CallSite")
        .map(|n| n.name.clone())
        .collect()
}

/// Defect B. A method invoked on a CALL RESULT inside a macro argument is a
/// real call and must be extracted. The macro scan's original signature was
/// `[identifier, identifier, token_tree]`; here the receiver is a
/// `token_tree` (the inner call's own arguments), so that signature cannot
/// match and the call was silently absent.
#[test]
fn a_method_on_a_call_result_inside_a_macro_is_extracted() {
    let sites = call_sites(
        "fn probe(d: u64) {\n    assert!(!Response::Unbounded(Unbounded::NonConvergent).meets(d));\n}\n",
    );
    assert!(
        sites.iter().any(|s| s.ends_with(".meets")),
        "no call site for `meets`; got {sites:?}"
    );
}

/// The extracted name must remain a CONTIGUOUS slice of the source ending on
/// the method identifier. `lsp_resolver::sites::lsp_position` adds
/// `last_segment_offset(callee_name)` to the stored column, so a name that is
/// not a contiguous slice ending at the method would aim the LSP request at
/// the wrong column and resolve to the wrong symbol, or to nothing.
#[test]
fn the_extracted_name_ends_on_the_method_identifier() {
    let sites = call_sites(
        "fn probe(d: u64) {\n    assert!(!Response::Unbounded(Unbounded::NonConvergent).meets(d));\n}\n",
    );
    let found = sites
        .iter()
        .find(|s| s.ends_with(".meets"))
        .expect("call site for `meets`");
    assert!(
        found.contains(").meets"),
        "name must span the receiver's closing paren through the method, got {found:?}"
    );
}

/// A method chained onto a call result, twice over, is two calls. This is the
/// shape that proves the scan resumes correctly after a match rather than
/// consuming the rest of the argument list.
#[test]
fn a_chain_on_a_call_result_inside_a_macro_yields_every_link() {
    let sites = call_sites("fn probe() {\n    assert!(build(1).first().second());\n}\n");
    assert!(
        sites.iter().any(|s| s.ends_with(".first")),
        "missing `first`; got {sites:?}"
    );
    assert!(
        sites.iter().any(|s| s.ends_with(".second")),
        "missing `second`; got {sites:?}"
    );
}

/// Two SEPARATE macro arguments must not be welded into a receiver call. The
/// bytes between them read `, `, never `.`, which is the only thing that can
/// tell the two apart once the grammar has made both separators anonymous.
#[test]
fn two_separate_macro_arguments_are_not_a_receiver_call() {
    let sites = call_sites("fn probe(flag: bool) {\n    assert!(flag, describe(1).text());\n}\n");
    assert!(
        !sites.iter().any(|s| s.contains("flag")),
        "a comma is not a receiver separator; got {sites:?}"
    );
}

/// Defect A. A bare identifier in argument position that names a PARAMETER of
/// the enclosing function is a value, not a function passed by reference. The
/// speculative by-value emission (#87) must skip it, otherwise a parameter
/// whose name collides with a real function anywhere in the codebase becomes a
/// false caller of it.
#[test]
fn a_parameter_used_as_an_argument_is_not_a_call() {
    let sites = call_sites(
        "fn task(c: u64, t: u64, d: u64) -> Task {\n    Task::new(c, t).deadline(d)\n}\n",
    );
    for name in ["c", "t", "d"] {
        assert!(
            !sites.iter().any(|s| s == name),
            "parameter `{name}` emitted as a call site; got {sites:?}"
        );
    }
}

/// The same rule for a local `let` binding: it is a value in scope, not a
/// function reference, and it collides with global names just as readily.
#[test]
fn a_local_binding_used_as_an_argument_is_not_a_call() {
    let sites = call_sites("fn probe() {\n    let t = 1;\n    consume(t);\n}\n");
    assert!(
        !sites.iter().any(|s| s == "t"),
        "local binding `t` emitted as a call site; got {sites:?}"
    );
}

/// The behaviour #87 exists for must survive: a genuine function passed by
/// value is neither a parameter nor a local binding here, so it is still
/// emitted. Without this the fix for A would silently delete the feature.
#[test]
fn a_function_passed_by_value_is_still_a_call_site() {
    let sites = call_sites("fn drain(queue: Vec<u8>) {\n    queue.iter().map(process_order);\n}\n");
    assert!(
        sites.iter().any(|s| s == "process_order"),
        "the #87 by-value reference was dropped; got {sites:?}"
    );
}

/// A destructuring `let` binds every name in its pattern, not only a simple
/// one. Without this the tuple names stay speculative call sites and collide
/// with any function sharing their name.
#[test]
fn a_destructured_let_binds_every_name_in_its_pattern() {
    let sites = call_sites("fn probe() {\n    let (first, second) = pair();\n    consume(first, second);\n}\n");
    for name in ["first", "second"] {
        assert!(
            !sites.iter().any(|s| s == name),
            "destructured binding `{name}` emitted as a call site; got {sites:?}"
        );
    }
}

/// The same for a struct pattern in PARAMETER position, which binds its field
/// names into the function scope.
#[test]
fn a_struct_pattern_parameter_binds_its_field_names() {
    let sites = call_sites("fn probe(Point { x, y }: Point) {\n    consume(x, y);\n}\n");
    for name in ["x", "y"] {
        assert!(
            !sites.iter().any(|s| s == name),
            "pattern-bound field `{name}` emitted as a call site; got {sites:?}"
        );
    }
}

/// A module-level constant initializer is not scanned for calls at all, so the
/// binding question never arises there. Measured rather than assumed: the
/// fixture below yields an empty set, not a call site for `compute`. Pinned
/// because it is the boundary of what the speculative scan can reach.
#[test]
fn a_module_level_initializer_yields_no_call_sites() {
    let sites = call_sites("const N: usize = compute(SEED);\n");
    assert!(
        sites.is_empty(),
        "module-level initializers are outside the call walk; got {sites:?}"
    );
}
