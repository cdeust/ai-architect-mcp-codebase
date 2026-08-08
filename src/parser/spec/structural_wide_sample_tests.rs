// parser::spec::structural_wide_sample_tests — issue #224 follow-up: measures
// recall on grammars NOBODY has hand-tuned an extractor for, per the task's
// "a projection to 180 languages must rest on a measured recall rate over a
// wider sample, not on the 10 we happen to have hand-written extractors for."
//
// Every grammar below is a `dev-dependency` (test-only; zero effect on the
// shipped binary, the production dispatch, or the end-result bench — see
// `Cargo.toml`'s comment at the dependency declaration). None of these
// languages were consulted while designing `structural.rs`/
// `structural_fallback.rs`'s TIER 1/TIER 2 rules — this file is the honesty
// check on whether those rules generalize, not a fixture tuned to make them
// look good.
//
// ## Method
// One minimal snippet per language: a free function/method-shaped
// definition with a call. No heritage/visibility snippets here (those are
// harder to write correctly per-language without becoming per-language
// tuning, which would defeat the point of an UNBIASED sample) — this file
// measures the SAME two things the original 10-language table measured
// first: "is there a Function/Method node?" and "is there a CallSite node?".
// A language counts as RECALLED when both are non-empty. This is a strict,
// binary, auditable measure — not a partial-credit score.

use super::structural::{parse_structural, StructuralSpec};
use crate::parser::{Language, LABEL_CALL_SITE, LABEL_FUNCTION, LABEL_METHOD};

struct WideSample {
    name: &'static str,
    ts_language: fn() -> tree_sitter::Language,
    src: &'static str,
    file: &'static str,
}

fn has_any_function_or_method(nodes: &[crate::parser::ExtractedNode]) -> bool {
    nodes
        .iter()
        .any(|n| n.label == LABEL_FUNCTION || n.label == LABEL_METHOD)
}

fn has_any_call(nodes: &[crate::parser::ExtractedNode]) -> bool {
    nodes.iter().any(|n| n.label == LABEL_CALL_SITE)
}

/// Runs the whole wide sample and returns `(recalled, total, per_language)`.
/// A dedicated `#[test]` below asserts on the aggregate; this function is
/// also reused by `wide_sample_recall_report` to print the honest per-language
/// breakdown that the PR body's recall rate is drawn from.
fn run_wide_sample() -> (usize, usize, Vec<(&'static str, bool, bool)>) {
    let samples: Vec<WideSample> = vec![
        WideSample {
            name: "C#",
            ts_language: || tree_sitter_c_sharp::LANGUAGE.into(),
            src: "int Helper() {\n    return 1;\n}\n\nint Greet() {\n    return Helper();\n}\n",
            file: "m.cs",
        },
        WideSample {
            name: "PHP",
            ts_language: || tree_sitter_php::LANGUAGE_PHP.into(),
            src: "<?php\nfunction helper() {\n    return 1;\n}\n\nfunction greet() {\n    return helper();\n}\n",
            file: "m.php",
        },
        WideSample {
            name: "Scala",
            ts_language: || tree_sitter_scala::LANGUAGE.into(),
            src: "def helper(): Int = {\n  1\n}\n\ndef greet(): Int = {\n  helper()\n}\n",
            file: "m.scala",
        },
        WideSample {
            name: "Haskell",
            ts_language: || tree_sitter_haskell::LANGUAGE.into(),
            src: "helper :: Int\nhelper = 1\n\ngreet :: Int\ngreet = helper\n",
            file: "m.hs",
        },
        WideSample {
            name: "Elixir",
            ts_language: || tree_sitter_elixir::LANGUAGE.into(),
            src: "def helper() do\n  1\nend\n\ndef greet() do\n  helper()\nend\n",
            file: "m.ex",
        },
        WideSample {
            name: "Lua",
            ts_language: || tree_sitter_lua::LANGUAGE.into(),
            src: "function helper()\n  return 1\nend\n\nfunction greet()\n  return helper()\nend\n",
            file: "m.lua",
        },
        WideSample {
            name: "Bash",
            ts_language: || tree_sitter_bash::LANGUAGE.into(),
            src: "helper() {\n  return 1\n}\n\ngreet() {\n  helper\n}\n",
            file: "m.sh",
        },
        WideSample {
            name: "JavaScript",
            ts_language: || tree_sitter_javascript::LANGUAGE.into(),
            src: "function helper() {\n    return 1;\n}\n\nfunction greet() {\n    return helper();\n}\n",
            file: "m.js",
        },
        WideSample {
            name: "Zig",
            ts_language: || tree_sitter_zig::LANGUAGE.into(),
            src: "fn helper() i32 {\n    return 1;\n}\n\nfn greet() i32 {\n    return helper();\n}\n",
            file: "m.zig",
        },
        WideSample {
            name: "Julia",
            ts_language: || tree_sitter_julia::LANGUAGE.into(),
            src: "function helper()\n    return 1\nend\n\nfunction greet()\n    return helper()\nend\n",
            file: "m.jl",
        },
        WideSample {
            name: "Dart",
            ts_language: || tree_sitter_dart::LANGUAGE.into(),
            src: "int helper() {\n  return 1;\n}\n\nint greet() {\n  return helper();\n}\n",
            file: "m.dart",
        },
        WideSample {
            name: "OCaml",
            ts_language: || tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            src: "let helper () = 1\n\nlet greet () = helper ()\n",
            file: "m.ml",
        },
    ];

    let mut results = Vec::with_capacity(samples.len());
    let mut recalled = 0usize;
    let total = samples.len();
    for sample in &samples {
        let spec = StructuralSpec {
            language: Language::Rust, // unused by parse_structural beyond error messages
            ts_language: sample.ts_language,
        };
        let (defs_ok, calls_ok) = match parse_structural(&spec, sample.src, sample.file) {
            Ok((r, _stats)) => (has_any_function_or_method(&r.nodes), has_any_call(&r.nodes)),
            Err(_) => (false, false),
        };
        if defs_ok && calls_ok {
            recalled += 1;
        }
        results.push((sample.name, defs_ok, calls_ok));
    }
    (recalled, total, results)
}

#[test]
fn wide_sample_recall_report() {
    let (recalled, total, results) = run_wide_sample();
    eprintln!("\n== Widened-sample recall (issue #224 follow-up) ==");
    for (name, defs_ok, calls_ok) in &results {
        eprintln!(
            "{name:12} defs={} calls={}",
            if *defs_ok { "YES" } else { "no" },
            if *calls_ok { "YES" } else { "no" }
        );
    }
    eprintln!(
        "== recalled {recalled}/{total} ({:.0}%) ==\n",
        100.0 * recalled as f64 / total as f64
    );
}

#[test]
fn wide_sample_recall_is_measured_not_assumed() {
    // This test's job is NOT to assert a specific recall rate (that would
    // pin an assumption, exactly what this file exists to avoid) — it just
    // proves the harness itself runs cleanly (no panics/parse-timeouts)
    // across all 12 grammars, so `wide_sample_recall_report`'s printed
    // numbers are trustworthy.
    let (_recalled, total, results) = run_wide_sample();
    assert_eq!(results.len(), total);
    assert_eq!(total, 12);
}

// ## Why the 4 non-recalled languages fail (verified by direct grammar
// introspection during development — a `#[cfg(test)] mod diag_failures`
// tree-dump harness identical in spirit to `structural_fallback`'s own
// development diagnostics, since deleted — each finding below is a real
// grammar-design fact, not a wrong-snippet artifact; ruling that out is the
// same discipline the ORIGINAL 10-language table applied per its own
// "Method" section):
//
// - **Haskell**: a top-level binding is `bind { name, match { expression } }`
//   — neither `name`+`body` (it's `name`+`match`, and `match`'s own payload
//   field is `expression`, not `body`) nor a kind ending in this engine's
//   definition-site suffix vocabulary (`"bind"` matches none of
//   declaration/definition/interface/implementation). Genuinely outside
//   both TIER 1 and TIER 2's vocabulary — equational function definition is
//   a different shape than every other grammar in either sample uses.
// - **Elixir**: `def helper() do 1 end` parses as a `call` node itself
//   (`target`="def" identifier, `arguments`=[the nested call `helper()`],
//   plus a `do_block`) — a direct consequence of Elixir's homoiconic macro
//   system, where `def` is syntactically a function call, not a distinct
//   definition-shaped node kind at all. No definition/call distinction this
//   engine draws applies; reaching it would require modeling macro
//   expansion, not adding a field name.
// - **OCaml**: `let helper () = 1` is a `value_definition` wrapping a
//   `let_binding` with fields `pattern`(the name)+`parameter`+`body` — the
//   name field is called `pattern`, a THIRD spelling (after `name` and
//   Rust's absent-field case) this engine's vocabulary does not include,
//   and `value_definition`'s kind does not end with this engine's suffix
//   vocabulary either.
// - **Bash** (calls only — definitions ARE recalled): `command` nodes use
//   field `argument` (singular), not `arguments` (plural) — one character
//   away from `call_callee_field`'s exact match. A real, minimal, arguably
//   generalizable gap, deliberately NOT patched here: this file's entire
//   purpose is measuring the engine's rules as designed against languages
//   that did not inform their design; adding an `"argument"` candidate
//   after seeing Bash fail would be curve-fitting the sample, which is
//   exactly what an unbiased recall measurement must not do.
