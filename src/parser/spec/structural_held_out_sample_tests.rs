// parser::spec::structural_held_out_sample_tests — issue #224's Elixir/Zig/
// Bash follow-up: the SECOND, genuinely out-of-sample recall measurement.
//
// ## Why this file exists, separately from `structural_wide_sample_tests`
// That file's 12-grammar sample named Elixir, Bash, and Zig individually as
// unresolved failures — the exact task that produced this follow-up's
// global-table extensions (`structural.rs`'s `target` branch and
// `ARGUMENTS_FIELD_CANDIDATES`, `structural_fallback.rs`'s fieldless-call
// tier, `structural_imports.rs`'s widened `IMPORT_CALL_NAMES`/
// `import_call_path`). Tuning global rules against a documented failure in
// that sample and then re-measuring recall ON that same sample would not be
// evidence of generalization — it would be curve-fitting reported as if it
// were still a held-out result. This file is a DISJOINT sample, selected
// AFTER those rules were frozen, and its numbers were not looked at while
// choosing the fixes above (verified by construction: every rule in
// `structural.rs`/`structural_fallback.rs`/`structural_imports.rs` cites
// Elixir/Zig/Bash's OWN grammars as its justification, never one of the ten
// languages below).
//
// ## Selection method: real-world PREVALENCE, not crates.io availability
// An owner correction (mid-task) replaced an initial by-availability
// selection (Perl/R/Nix/Erlang/Clojure/Fortran/Vim/Solidity/Verilog/...)
// with one anchored to GitHub Linguist-ranked real-world usage — a recall
// figure measured on obscure languages predicts nothing about a 180-language
// rollout dominated by widely-deployed ones. Ten languages below, each
// chosen because it ranks among the highest-prevalence languages NOT already
// covered by the original 10+Ruby (`structural_coverage_tests`) or the
// tuned-against 12 (`structural_wide_sample_tests`): SQL, HTML, CSS,
// PowerShell, R, Perl, Groovy, YAML, Dockerfile, HCL(Terraform).
//
// **Not obtained**: Visual Basic/VB.NET has no published tree-sitter grammar
// crate on crates.io (searched under `tree-sitter-vb` and
// `tree-sitter-visualbasic`, both absent, verified 2026-08-09) — reported as
// genuinely unobtainable, not silently dropped. MATLAB, Assembly, COBOL, and
// Pascal DO have grammar crates available but were left out of this sample
// to keep it scoped to the primary prevalence-ranked list the owner
// prioritized; adding them is a natural extension for a future sample, not
// a gap in THIS one's honesty.
//
// **Two crate substitutions, both verified, documented in `Cargo.toml`**:
// the official `tree-sitter-sql` (0.0.2) and `tree-sitter-dockerfile`
// (0.2.0) both pin an OLD `pub fn language() -> tree_sitter::Language`
// binding tied to their own ancient `tree-sitter` dependency (0.19/0.20) —
// verified this does not compile against this crate's `tree-sitter` 0.26 at
// all (a type-level incompatibility, not a version-range issue). Replaced
// with actively-maintained forks using the modern `LanguageFn` shim:
// `tree-sitter-sequel` (SQL) and `tree-sitter-containerfile` (Dockerfile/
// Containerfile), same grammar lineage, newer binding only.
//
// ## Method — same strict binary metric as `structural_wide_sample_tests`,
// PLUS an honest applicability flag the owner explicitly required
// A language counts as `defs_ok`/`calls_ok` when the engine emits ANY
// Function/Method node, resp. ANY CallSite node, from its representative
// snippet — unchanged from the original metric. NEW: `defs_applicable` and
// `calls_applicable` record whether the language's OWN grammar has a
// function-DEFINITION construct and a function/procedure-CALL construct AT
// ALL, verified by direct grammar introspection (the same discipline
// `structural_wide_sample_tests`'s "why they fail" comment already applies)
// — NOT by whether this engine happens to reach it. Several of the ten have
// no functions or classes whatsoever (HTML, YAML, Dockerfile are pure
// declarative/markup languages with no callable-unit concept in their
// grammar at all; SQL and HCL/Terraform partially do — SQL has
// `CREATE FUNCTION` and invocations, HCL has built-in function CALLS in
// expressions but no user function-DEFINITION construct; CSS similarly has
// built-in function calls, `rgba()`/`url()`, but no function-definition
// construct). A language where `defs_applicable`/`calls_applicable` is
// `false` is EXCLUDED from the strict recall denominator for that column —
// counting it as either a pass or a fail would corrupt the number, per the
// owner's explicit instruction. The same `Some`/`None` split applies to
// imports: `import_applicable` records whether the language has an
// import-equivalent construct in its own grammar (SQL and HCL/Terraform
// core config files do not; the rest do).

use super::structural::{parse_structural, StructuralSpec};
use crate::parser::{Language, LABEL_CALL_SITE, LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD};

struct HeldOutSample {
    name: &'static str,
    ts_language: fn() -> tree_sitter::Language,
    src: &'static str,
    file: &'static str,
    /// Whether this language's OWN grammar has a function/method-DEFINITION
    /// construct at all — verified by direct introspection, independent of
    /// whether this engine reaches it. `false` excludes the language from
    /// the `defs` column of the strict recall denominator.
    defs_applicable: bool,
    /// Same, for a function/procedure-CALL construct.
    calls_applicable: bool,
    /// A minimal, real, idiomatic import-equivalent statement, or `None`
    /// when the language's grammar has no import construct at all
    /// (verified) — mirrors `structural_wide_sample_tests`'s own
    /// `import_src: Option<&str>` convention.
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

fn held_out_samples() -> Vec<HeldOutSample> {
    vec![
        HeldOutSample {
            name: "SQL",
            ts_language: || tree_sitter_sequel::LANGUAGE.into(),
            src: "CREATE FUNCTION helper() RETURNS INT AS $$ SELECT 1; $$ LANGUAGE SQL;\nSELECT helper();\n",
            file: "m.sql",
            defs_applicable: true,
            calls_applicable: true,
            // No standard IMPORT construct in generic/PostgreSQL-dialect
            // SQL (verified: no anonymous "import" keyword, no equivalent
            // construct in `tree-sitter-sequel`'s grammar).
            import_src: None,
        },
        HeldOutSample {
            name: "HTML",
            ts_language: || tree_sitter_html::LANGUAGE.into(),
            src: "<html><body><p>hi</p></body></html>\n",
            file: "m.html",
            // Pure markup: verified via direct introspection that HTML's
            // grammar has no function/method/call node kind at all —
            // `element`/`start_tag`/`end_tag`/`text` only.
            defs_applicable: false,
            calls_applicable: false,
            import_src: None,
        },
        HeldOutSample {
            name: "CSS",
            ts_language: || tree_sitter_css::LANGUAGE.into(),
            src: ".foo { color: rgba(255, 0, 0, 1); }\n",
            file: "m.css",
            // CSS has no user function-DEFINITION construct at all (no
            // `@function`-defines-a-name-you-call convention in CSS3) —
            // `defs_applicable = false`. It DOES have function CALLS
            // (`rgba(...)`, `calc(...)`, `url(...)`) via `call_expression`.
            defs_applicable: false,
            calls_applicable: true,
            // `@import url("foo.css");` exists.
            import_src: Some("@import url(\"foo.css\");\n"),
        },
        HeldOutSample {
            name: "PowerShell",
            ts_language: || tree_sitter_powershell::LANGUAGE.into(),
            src: "function Helper {\n    return 1\n}\n\nfunction Greet {\n    Helper\n}\n",
            file: "m.ps1",
            defs_applicable: true,
            calls_applicable: true,
            // `Import-Module` is PowerShell's real import convention.
            import_src: Some("Import-Module MyModule\n"),
        },
        HeldOutSample {
            name: "R",
            ts_language: || tree_sitter_r::LANGUAGE.into(),
            src: "helper <- function() {\n  1\n}\n\ngreet <- function() {\n  helper()\n}\n",
            file: "m.r",
            defs_applicable: true,
            calls_applicable: true,
            // `library(pkg)` is R's real import convention.
            import_src: Some("library(dplyr)\n"),
        },
        HeldOutSample {
            name: "Perl",
            ts_language: || tree_sitter_perl::LANGUAGE.into(),
            src: "sub helper {\n    return 1;\n}\n\nsub greet {\n    return helper();\n}\n",
            file: "m.pl",
            defs_applicable: true,
            calls_applicable: true,
            import_src: Some("use strict;\n"),
        },
        HeldOutSample {
            name: "Groovy",
            ts_language: || tree_sitter_groovy::LANGUAGE.into(),
            src: "int helper() {\n    return 1\n}\n\nint greet() {\n    return helper()\n}\n",
            file: "m.groovy",
            defs_applicable: true,
            calls_applicable: true,
            import_src: Some("import java.util.List\n"),
        },
        HeldOutSample {
            name: "YAML",
            ts_language: || tree_sitter_yaml::LANGUAGE.into(),
            src: "key: value\nlist:\n  - a\n  - b\n",
            file: "m.yaml",
            // Pure data-serialization: verified no function/call/import
            // node kind exists in the grammar at all.
            defs_applicable: false,
            calls_applicable: false,
            import_src: None,
        },
        HeldOutSample {
            name: "Dockerfile",
            ts_language: || tree_sitter_containerfile::LANGUAGE.into(),
            src: "FROM ubuntu:22.04\nRUN apt-get update\nCOPY . /app\n",
            file: "Dockerfile",
            // Build instructions (`from_instruction`/`run_instruction`/
            // `copy_instruction`), not functions or calls in this engine's
            // sense — verified via direct introspection, no kind matching
            // either vocabulary.
            defs_applicable: false,
            calls_applicable: false,
            import_src: None,
        },
        HeldOutSample {
            name: "HCL",
            ts_language: || tree_sitter_hcl::LANGUAGE.into(),
            src: "resource \"aws_instance\" \"foo\" {\n  ami = file(\"path\")\n}\n",
            file: "m.tf",
            // HCL/Terraform has no user function-DEFINITION construct in
            // core config files (`resource`/`variable`/`data` are
            // declarative BLOCKS, fieldless, matching neither this engine's
            // definition vocabulary nor a plausible extension of it). It
            // DOES have built-in function CALLS in expressions (`file(...)`
            // above) via a `function_call` node.
            defs_applicable: false,
            calls_applicable: true,
            // No `import` keyword/statement in core HCL/Terraform config
            // files (verified: `terraform import` is a CLI command, not a
            // config-file construct; the `import` BLOCK is a distinct,
            // rarer HCL feature not exercised by this minimal sample).
            import_src: None,
        },
    ]
}

/// Mirrors `structural_wide_sample_tests::run_wide_sample`, but returns
/// results annotated with each language's `(defs_applicable,
/// calls_applicable)` flags so the caller can compute a recall rate over
/// only the languages where the metric structurally applies.
fn run_held_out_sample() -> Vec<(&'static str, bool, bool, bool, bool)> {
    held_out_samples()
        .iter()
        .map(|sample| {
            let spec = StructuralSpec {
                language: Language::Rust, // unused by parse_structural beyond error messages
                ts_language: sample.ts_language,
            };
            let (defs_ok, calls_ok) = match parse_structural(&spec, sample.src, sample.file) {
                Ok((r, _stats)) => (has_any_function_or_method(&r.nodes), has_any_call(&r.nodes)),
                Err(_) => (false, false),
            };
            (
                sample.name,
                defs_ok,
                calls_ok,
                sample.defs_applicable,
                sample.calls_applicable,
            )
        })
        .collect()
}

fn run_held_out_sample_imports() -> Vec<(&'static str, bool)> {
    held_out_samples()
        .iter()
        .filter_map(|sample| {
            let import_src = sample.import_src?;
            let spec = StructuralSpec {
                language: Language::Rust,
                ts_language: sample.ts_language,
            };
            let import_ok = match parse_structural(&spec, import_src, sample.file) {
                Ok((r, _stats)) => has_any_import(&r.nodes),
                Err(_) => false,
            };
            Some((sample.name, import_ok))
        })
        .collect()
}

#[test]
fn held_out_sample_recall_report() {
    let results = run_held_out_sample();
    eprintln!("\n== Held-out prevalence-ranked sample recall (issue #224 follow-up) ==");
    let mut defs_applicable_total = 0usize;
    let mut defs_recalled = 0usize;
    let mut calls_applicable_total = 0usize;
    let mut calls_recalled = 0usize;
    let mut both_applicable_total = 0usize;
    let mut both_recalled = 0usize;
    for (name, defs_ok, calls_ok, defs_applicable, calls_applicable) in &results {
        let defs_field = if !defs_applicable {
            "n/a".to_string()
        } else {
            (if *defs_ok { "YES" } else { "no" }).to_string()
        };
        let calls_field = if !calls_applicable {
            "n/a".to_string()
        } else {
            (if *calls_ok { "YES" } else { "no" }).to_string()
        };
        eprintln!("{name:12} defs={defs_field:4} calls={calls_field:4}");
        if *defs_applicable {
            defs_applicable_total += 1;
            if *defs_ok {
                defs_recalled += 1;
            }
        }
        if *calls_applicable {
            calls_applicable_total += 1;
            if *calls_ok {
                calls_recalled += 1;
            }
        }
        if *defs_applicable && *calls_applicable {
            both_applicable_total += 1;
            if *defs_ok && *calls_ok {
                both_recalled += 1;
            }
        }
    }
    eprintln!(
        "== defs recalled {defs_recalled}/{defs_applicable_total} ({:.0}%) among languages where the metric applies ==",
        100.0 * defs_recalled as f64 / defs_applicable_total as f64
    );
    eprintln!(
        "== calls recalled {calls_recalled}/{calls_applicable_total} ({:.0}%) among languages where the metric applies ==",
        100.0 * calls_recalled as f64 / calls_applicable_total as f64
    );
    eprintln!(
        "== defs-AND-calls (strict) recalled {both_recalled}/{both_applicable_total} ({:.0}%) among the {both_applicable_total}/10 languages where BOTH apply — the honest 180-language predictor ==\n",
        100.0 * both_recalled as f64 / both_applicable_total as f64
    );

    let import_results = run_held_out_sample_imports();
    let import_applicable_total = import_results.len();
    let import_recalled = import_results.iter().filter(|(_, ok)| *ok).count();
    eprintln!("== Held-out sample IMPORTS recall ==");
    for (name, import_ok) in &import_results {
        eprintln!("{name:12} import={}", if *import_ok { "YES" } else { "no" });
    }
    eprintln!(
        "== imports recalled {import_recalled}/{import_applicable_total} ({:.0}%) among the {import_applicable_total}/10 languages where an import construct exists ==\n",
        100.0 * import_recalled as f64 / import_applicable_total as f64
    );
}

#[test]
fn held_out_sample_recall_is_measured_not_assumed() {
    let results = run_held_out_sample();
    assert_eq!(results.len(), 10);
}

#[test]
fn held_out_sample_imports_recall_is_measured_not_assumed() {
    // 5/10 languages have a verified import-equivalent construct (CSS,
    // PowerShell, R, Perl, Groovy); SQL, HTML, YAML, Dockerfile, and HCL do
    // not (each `import_src: None`, verified per-language above). Pinned
    // here so a future edit to `held_out_samples()` that silently drops an
    // `import_src` is caught structurally, not just by eyeballing the
    // report.
    let results = run_held_out_sample_imports();
    assert_eq!(results.len(), 5);
}

// ## Why the metric is inapplicable for HTML/YAML/Dockerfile (verified via
// direct grammar introspection, same discipline as
// `structural_wide_sample_tests`'s own failure analysis):
//   - **HTML**: a pure markup language — `element`/`start_tag`/`end_tag`/
//     `text`/`attribute` node kinds only, no function, call, or import
//     construct exists in the grammar at all.
//   - **YAML**: a pure data-serialization language — `block_mapping`/
//     `block_sequence`/`flow_node`/`plain_scalar` node kinds only, same
//     verdict.
//   - **Dockerfile**: a sequence of build INSTRUCTIONS
//     (`from_instruction`/`run_instruction`/`copy_instruction`/...), not
//     function definitions or calls; `RUN <shell command>` is the closest
//     analog to a "call" but is structurally a directive with a raw shell
//     string argument, not a call-shaped node by any grammar convention
//     this engine's vocabulary (or a plausible extension of it) recognizes.
//
// ## Why CSS's and HCL's `defs` column is inapplicable, and their real gaps
//   - **CSS**: no function-DEFINITION construct exists in CSS at all (only
//     built-in functions you CALL — `rgba()`, `calc()`, `url()`). Its CALLS
//     (`defs_applicable=false`, `calls_applicable=true`) ARE recalled — via
//     `structural_fallback.rs`'s fieldless-call tier (`call_expression` has
//     zero declared fields, a positional `arguments`-kind child), the SAME
//     generic mechanism added for Zig, not a CSS-specific accommodation.
//     CSS's `@import url(...)` is NOT recalled: its anonymous keyword kind
//     is literally `"@import"` (with the `@`), not `"import"` — outside
//     `IMPORT_KEYWORD_TOKENS`' current vocabulary — and the nested
//     `url(...)` call's callee text `"url"` is not in `IMPORT_CALL_NAMES`.
//     Both are plausible, small, ADDITIONAL global-table entries — reported
//     here, deliberately NOT added, because this file's entire purpose is
//     measuring the frozen rules against a sample that did not inform them;
//     patching after seeing this specific failure would be exactly the
//     curve-fitting this file exists to avoid.
//   - **HCL**: no function-DEFINITION construct in core Terraform config
//     (`resource`/`variable`/`data` blocks are declarative and fieldless).
//     Its one built-in-function CALL construct (`file("path")` above) is
//     NOT recalled: HCL's argument-list node is named `"function_arguments"`,
//     not `"arguments"` — one more real, plausible, deliberately UNPATCHED
//     vocabulary gap for the same reason as CSS's above.
//
// ## Why SQL and PowerShell (both fully applicable) miss BOTH columns
//   - **SQL**: `CREATE FUNCTION helper() RETURNS INT AS $$ ... $$ LANGUAGE
//     SQL` nests the function name three levels deep
//     (`create_function.object_reference.name`, not a direct `name` field
//     on `create_function` itself) with NO `body` field at all (the SQL
//     body lives in a positional `function_body` child) — outside both this
//     engine's field-shape vocabulary and its kind-suffix vocabulary
//     (`"create_function"` does not end with declaration/definition/
//     interface/implementation). `SELECT helper()`'s `invocation` node has
//     a positional (not fielded) `object_reference` and no `arguments`-kind
//     child at all for a zero-argument call — outside every mechanism this
//     engine adds, including the fieldless-call tier (which requires an
//     actual `arguments`-kind child to exist).
//   - **PowerShell**: `function_statement` has ZERO declared fields (its
//     name and body are POSITIONAL children of kind `function_name`/
//     `script_block`, not `name`/`body` fields) and its kind ends with
//     `"statement"`, outside this engine's definition-site suffix
//     vocabulary. Its `command` node (both `Helper` and `Import-Module
//     MyModule`) is likewise fieldless with no `arguments`-kind child
//     (its argument list is `command_elements`, a different kind name
//     entirely) — outside the fieldless-call tier's own vocabulary too.
//
// ## The one genuine quality caveat inside an otherwise-recalled language
//   - **R**: `helper <- function() { 1 }` is a `binary_operator`
//     (assignment) wrapping a `function_definition` — this engine reaches
//     the NESTED `function_definition` node directly (the whole-tree walk
//     visits every node, not just top-level ones), and R's grammar quirk
//     assigns the FIELD NAME `"name"` to the literal `function` KEYWORD
//     TOKEN itself (not to `helper`, which lives on the assignment's `lhs`,
//     outside the function node entirely) — so R's defs ARE recalled by
//     this engine's strict label-only metric, but with the wrong NAME
//     ("function" instead of "helper") on every single one. Reported
//     honestly rather than folded silently into a recall win: the strict
//     metric only checks for label presence, not name correctness, and this
//     is exactly the kind of gap that metric cannot see.
