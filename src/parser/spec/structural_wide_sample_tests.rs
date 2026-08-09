// parser::spec::structural_wide_sample_tests — issue #224 follow-up: measures
// recall on grammars NOBODY has hand-tuned an extractor for, per the task's
// "a projection to 180 languages must rest on a measured recall rate over a
// wider sample, not on the 10 we happen to have hand-written extractors for."
//
// Every grammar below is a `dev-dependency` (test-only; zero effect on the
// shipped binary, the production dispatch, or the end-result bench — see
// `Cargo.toml`'s comment at the dependency declaration). None of these
// languages were consulted while designing `structural.rs`/
// `structural_fallback.rs`'s original TIER 1/TIER 2 rules (PR #225/#226).
//
// ## THIS SAMPLE IS NOW TUNED-AGAINST — do not read its recall number as an
// out-of-sample projection
// Issue #224's Elixir/Zig/Bash follow-up (this file's own documented
// failures from PR #226/#230) is the reason those three languages' import
// rules exist at all — Elixir/Bash/Zig were named, individually, as the
// task that produced the global-table extensions in `structural.rs`/
// `structural_fallback.rs`/`structural_imports.rs`. That makes THIS sample a
// training set for that follow-up's rules, not a held-out one, even though
// the rules are still expressed as global tables, never per-language code.
// A NEW, disjoint, prevalence-ranked held-out sample
// (`structural_held_out_sample_tests.rs`) was added and run AFTER those
// rules were frozen, to measure genuine out-of-sample generalization — see
// that file's own module doc for the honest 180-language predictor.
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
use crate::parser::{Language, LABEL_CALL_SITE, LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD};

struct WideSample {
    name: &'static str,
    ts_language: fn() -> tree_sitter::Language,
    src: &'static str,
    file: &'static str,
    /// A minimal, real, idiomatic import statement for this language,
    /// measured SEPARATELY from `src` (issue #224's imports follow-up: the
    /// def/call snippet above never contains an import, so recall needs its
    /// own source). `None` marks a language with no single-statement import
    /// construct simple enough to isolate without also pulling in unrelated
    /// syntax (none in this sample — kept as an `Option` so a future
    /// addition to this table isn't forced to invent one).
    import_src: Option<&'static str>,
}

fn has_any_function_or_method(nodes: &[crate::parser::ExtractedNode]) -> bool {
    nodes
        .iter()
        .any(|n| n.label == LABEL_FUNCTION || n.label == LABEL_METHOD)
}

fn has_any_call(nodes: &[crate::parser::ExtractedNode]) -> bool {
    nodes.iter().any(|n| n.label == LABEL_CALL_SITE)
}

fn has_any_import(nodes: &[crate::parser::ExtractedNode]) -> bool {
    nodes.iter().any(|n| n.label == LABEL_IMPORT)
}

/// Runs the whole wide sample and returns `(recalled, total, per_language)`.
/// A dedicated `#[test]` below asserts on the aggregate; this function is
/// also reused by `wide_sample_recall_report` to print the honest per-language
/// breakdown that the PR body's recall rate is drawn from.
fn wide_samples() -> Vec<WideSample> {
    vec![
        WideSample {
            name: "C#",
            ts_language: || tree_sitter_c_sharp::LANGUAGE.into(),
            src: "int Helper() {\n    return 1;\n}\n\nint Greet() {\n    return Helper();\n}\n",
            file: "m.cs",
            import_src: Some("using System.Collections.Generic;\n"),
        },
        WideSample {
            name: "PHP",
            ts_language: || tree_sitter_php::LANGUAGE_PHP.into(),
            src: "<?php\nfunction helper() {\n    return 1;\n}\n\nfunction greet() {\n    return helper();\n}\n",
            file: "m.php",
            import_src: Some("<?php\nuse Foo\\Bar;\n"),
        },
        WideSample {
            name: "Scala",
            ts_language: || tree_sitter_scala::LANGUAGE.into(),
            src: "def helper(): Int = {\n  1\n}\n\ndef greet(): Int = {\n  helper()\n}\n",
            file: "m.scala",
            import_src: Some("import scala.collection.mutable\n"),
        },
        WideSample {
            name: "Haskell",
            ts_language: || tree_sitter_haskell::LANGUAGE.into(),
            src: "helper :: Int\nhelper = 1\n\ngreet :: Int\ngreet = helper\n",
            file: "m.hs",
            import_src: Some("import Data.List\n"),
        },
        WideSample {
            name: "Elixir",
            ts_language: || tree_sitter_elixir::LANGUAGE.into(),
            src: "def helper() do\n  1\nend\n\ndef greet() do\n  helper()\nend\n",
            file: "m.ex",
            // CLOSED (issue #224's Elixir follow-up): `import Enum` parses
            // as a `call` node with FIELD `target`, now a recognized callee
            // field (`structural.rs`'s `call_callee_field`, gated on an
            // `arguments`-KIND child to stay safe against Swift's own
            // `target`-field nodes); `IMPORT_CALL_NAMES` then flags the
            // classified call's `target` text ("import") as also an import.
            // `def`/ordinary calls (`helper()`) reach `CallSite` the same
            // way but are NOT definitions — Elixir's homoiconic `def` stays
            // unreached as a Def, unchanged, see the trailing comment below.
            import_src: Some("import Enum\n"),
        },
        WideSample {
            name: "Lua",
            ts_language: || tree_sitter_lua::LANGUAGE.into(),
            src: "function helper()\n  return 1\nend\n\nfunction greet()\n  return helper()\nend\n",
            file: "m.lua",
            import_src: Some("local foo = require('foo')\n"),
        },
        WideSample {
            name: "Bash",
            ts_language: || tree_sitter_bash::LANGUAGE.into(),
            src: "helper() {\n  return 1\n}\n\ngreet() {\n  helper\n}\n",
            file: "m.sh",
            // CLOSED (issue #224's Bash follow-up): `source ./foo.sh` is a
            // `command` node (`name`+`argument` fields, singular
            // `argument`) — `structural.rs`'s `ARGUMENTS_FIELD_CANDIDATES`
            // now accepts the singular spelling too, so the node classifies
            // as an ordinary `name`+argument CallSite (callee "source");
            // `IMPORT_CALL_NAMES` then flags it as also an import. The same
            // one-character widening is what flips Bash's ordinary-call
            // recall below (`calls=YES`, previously `no`).
            import_src: Some("source ./foo.sh\n"),
        },
        WideSample {
            name: "JavaScript",
            ts_language: || tree_sitter_javascript::LANGUAGE.into(),
            src: "function helper() {\n    return 1;\n}\n\nfunction greet() {\n    return helper();\n}\n",
            file: "m.js",
            import_src: Some("import { foo } from 'bar';\n"),
        },
        WideSample {
            name: "Zig",
            ts_language: || tree_sitter_zig::LANGUAGE.into(),
            src: "fn helper() i32 {\n    return 1;\n}\n\nfn greet() i32 {\n    return helper();\n}\n",
            file: "m.zig",
            // CLOSED (issue #224's Zig follow-up): `@import("std")` is a
            // `builtin_function` node with ZERO fields at all —
            // `structural_fallback.rs`'s fieldless-call tier
            // (`is_fieldless_call_with_positional_arguments`) now recognizes
            // any node whose children are ALL positional (no field-tagged
            // child) but that carries an `arguments`-kind child as a call,
            // callee = the first named child (`@import`, the
            // `builtin_identifier`); `IMPORT_CALL_NAMES` then flags it.
            import_src: Some("const std = @import(\"std\");\n"),
        },
        WideSample {
            name: "Julia",
            ts_language: || tree_sitter_julia::LANGUAGE.into(),
            src: "function helper()\n    return 1\nend\n\nfunction greet()\n    return helper()\nend\n",
            file: "m.jl",
            import_src: Some("using Foo\n"),
        },
        WideSample {
            name: "Dart",
            ts_language: || tree_sitter_dart::LANGUAGE.into(),
            src: "int helper() {\n  return 1;\n}\n\nint greet() {\n  return helper();\n}\n",
            file: "m.dart",
            import_src: Some("import 'dart:core';\n"),
        },
        WideSample {
            name: "OCaml",
            ts_language: || tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            src: "let helper () = 1\n\nlet greet () = helper ()\n",
            file: "m.ml",
            import_src: Some("open Foo\n"),
        },
    ]
}

/// Runs the whole wide sample and returns `(recalled, total, per_language)`.
/// A dedicated `#[test]` below asserts on the aggregate; this function is
/// also reused by `wide_sample_recall_report` to print the honest per-language
/// breakdown that the PR body's recall rate is drawn from.
fn run_wide_sample() -> (usize, usize, Vec<(&'static str, bool, bool)>) {
    let samples = wide_samples();
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

/// Issue #224 follow-up: the imports column, measured SEPARATELY from
/// `run_wide_sample`'s defs/calls metric (a different snippet, a different
/// pass/fail criterion — "is there ANY `Import` node") rather than folded
/// into the existing pinned defs-AND-calls binary, so neither measurement's
/// history is disturbed by adding the other.
fn run_wide_sample_imports() -> (usize, usize, Vec<(&'static str, bool)>) {
    let samples: Vec<WideSample> = wide_samples();
    let mut results = Vec::with_capacity(samples.len());
    let mut recalled = 0usize;
    let total = samples.len();
    for sample in &samples {
        let Some(import_src) = sample.import_src else {
            continue;
        };
        let spec = StructuralSpec {
            language: Language::Rust, // unused by parse_structural beyond error messages
            ts_language: sample.ts_language,
        };
        let import_ok = match parse_structural(&spec, import_src, sample.file) {
            Ok((r, _stats)) => has_any_import(&r.nodes),
            Err(_) => false,
        };
        if import_ok {
            recalled += 1;
        }
        results.push((sample.name, import_ok));
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

    let (imports_recalled, imports_total, import_results) = run_wide_sample_imports();
    eprintln!("== Widened-sample IMPORTS recall (issue #224 imports follow-up) ==");
    for (name, import_ok) in &import_results {
        eprintln!("{name:12} import={}", if *import_ok { "YES" } else { "no" });
    }
    eprintln!(
        "== imports recalled {imports_recalled}/{imports_total} ({:.0}%) ==\n",
        100.0 * imports_recalled as f64 / imports_total as f64
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

#[test]
fn wide_sample_imports_recall_is_measured_not_assumed() {
    let (_recalled, total, results) = run_wide_sample_imports();
    assert_eq!(results.len(), total);
    assert_eq!(total, 12);
}

// ## Why the remaining non-recalled def/call cases fail (verified by direct
// grammar introspection — a `#[cfg(test)] mod diag_vocab_224` tree-dump
// harness identical in spirit to `structural_fallback`'s own development
// diagnostics, since deleted — each finding below is a real grammar-design
// fact, not a wrong-snippet artifact; ruling that out is the same discipline
// the ORIGINAL 10-language table applied per its own "Method" section).
//
// Issue #224's Elixir/Zig/Bash follow-up CLOSED the three-language import
// wall (12/12 = 100% on this sample, up from 9/12 = 75%) and Bash's
// def/call gap (`calls` now YES, via the same `argument`-singular field
// widening) through GLOBAL table extensions only — see
// `structural.rs`/`structural_fallback.rs`/`structural_imports.rs`'s module
// docs for the mechanism. Two genuinely different-shaped gaps remain, NOT
// patched, because no global rule reaches them without becoming
// per-construct special-casing:
//
// - **Haskell**: a top-level binding is `bind { name, match { expression } }`
//   — neither `name`+`body` (it's `name`+`match`, and `match`'s own payload
//   field is `expression`, not `body`) nor a kind ending in this engine's
//   definition-site suffix vocabulary (`"bind"` matches none of
//   declaration/definition/interface/implementation). Genuinely outside
//   both TIER 1 and TIER 2's vocabulary — equational function definition is
//   a different shape than every other grammar in either sample uses.
// - **OCaml**: `let helper () = 1` is a `value_definition` wrapping a
//   `let_binding` with fields `pattern`(the name)+`parameter`+`body` — the
//   name field is called `pattern`, a THIRD spelling (after `name` and
//   Rust's absent-field case) this engine's vocabulary does not include,
//   and `value_definition`'s kind does not end with this engine's suffix
//   vocabulary either. (OCaml's CALLS are now recalled as a side effect of
//   the Elixir fix — `application_expression` has both `function`+`argument`
//   fields, matching `call_callee_field`'s widened arguments-field check —
//   but its DEFINITIONS remain unreached, so the combined defs-AND-calls
//   metric below still counts it as a miss.)
// - **Elixir's `def`/`defmodule`/ordinary calls are Calls, not Defs**:
//   `def helper() do 1 end` parses as a `call` node itself (`target`="def"
//   identifier, positional `arguments`=[the nested call `helper()`], plus a
//   `do_block` child) — a direct consequence of Elixir's homoiconic macro
//   system, where `def` is syntactically a function call, not a distinct
//   definition-shaped node kind at all. The follow-up's `target`-field
//   branch correctly reaches this node as a CallSite (callee "def") — which
//   is the honest classification of what the node structurally IS — but no
//   definition/call distinction this engine draws can ALSO recover a
//   Function/Method node from it without modeling macro expansion, which is
//   a different, harder problem than a field-name gap. Elixir's `calls=YES`
//   below reflects `helper()` being reached as an ordinary call; `defs=no`
//   is this same, now fully-understood, boundary.
