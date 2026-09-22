# Independent DY-WCET source oracle

Raw assignment: "Construct independent source-grounded oracle before seeing graph results: all Rust fn definitions (file/name/line), #[test] definitions, five #[kani::proof] harnesses, and all distinct containing functions that call TaskSet::response_of (including macros; distinguish unrelated identifiers)."

## Execution contract

| Reference | Binding | Evidence |
|---|---|---|
| Corpus | DYResearch/dy-wcet at `1e93ccd462af2b2531c32e4ac9eae5bd8e95fb2b` | `git rev-parse HEAD`; clean initial `git status --porcelain` |
| Tests | Explicit `#[test]` functions in `src/lib.rs`, `tests/on_paper.rs`, `tests/properties.rs` | Full declaration/attribute lexical inventory |
| Kani proofs | Five annotated functions in `kani/response_bounds.rs` | Direct full-file read; execution status unknown |
| Callers | Direct calls resolving by local declarations to `TaskSet::response_of` | `response_of` occurrence search plus direct source review |

Symptom: verifier accuracy has no independent source denominator yet. Goal: persist an auditable inventory with file hashes and exact line references. Non-goals: run builds/tests, modify the corpus, infer proof success, or establish scheduling-theory correctness.

Strategy: context engineering plus verified reasoning, with source text as the external signal. Acceptance: pinned commit matches; all explicit fn tokens and test/proof annotations reconcile with inventory; all non-comment method-call occurrences reconcile with callsites; saved source excerpts match original lines. All these checks passed. No graph results were inspected while building this oracle. Cortex query/recall tools were absent from this worker's callable tool metadata; no memory was fabricated.

## Denominators

| File | Explicit functions | Ordinary tests | Kani harnesses | response_of callsites | Distinct containing callers |
|---|---:|---:|---:|---:|---:|
| src/lib.rs | 57 | 26 | 0 | 20 | 17 |
| tests/on_paper.rs | 15 | 15 | 0 | 16 | 12 |
| tests/properties.rs | 11 | 8 | 0 | 11 | 5 |
| kani/response_bounds.rs | 6 | 0 | 5 | 5 | 4 |
| Total | 89 | 49 | 5 | 52 | 38 |

There are 30 explicit production methods in `src/lib.rs`, plus 26 tests and one test helper. The four production callers of `TaskSet::response_of` are `is_schedulable` (declaration 511, call 513), `first_failure` (522, 523), `slack_of` (532, 534), and `optimal_priority_order` (624, 654). `first_failure` calls from inside a closure; its containing named function is the caller. `max_wcet_increase` calls `is_schedulable`, so it is an indirect dependent and is excluded from the direct caller denominator.

Method calls inside `assert_eq!`, `assert!`, and `matches!` count. For example `src/lib.rs:728` contains two calls, from receivers `a` and `b`, but one caller. All direct receivers were reviewed against local `TaskSet::new()` constructions or `generate` returning `TaskSet`; there are no unrelated direct `response_of` method calls in these four files. The Rustdoc example at `src/lib.rs:72` is outside any explicit source function and is excluded. Doc prose links and the declaration at line 453 are also excluded. Derived methods, anonymous closures, and rustdoc-generated functions are outside the explicit function denominator. Duplicate names are distinguished by file, scope, and declaration line, especially the three Display `fmt` implementations and duplicated test names.

## Kani source scope and wiring

| Harness declaration | Explicit unwind | Actual input/property scope |
|---|---:|---|
| response_bounds.rs:27 a_bounded_response_never_exceeds_its_deadline | 5 | One admitted task; `0 < t < 1_000_000`, `c <= d < 1_000_000`, `b,j < 1_000_000`; if output is Bounded, assert `r <= d`. |
| response_bounds.rs:48 every_unbounded_variant_fails_every_deadline | none specified | Arbitrary u64 deadline/value; four manually constructed Unbounded variants fail `meets`. Does not call response_of. |
| response_bounds.rs:61 the_recurrence_terminates_without_panicking | 5 | Two admitted tasks; arbitrary u64 values constrained to positive periods and execution <= period; deadline=period, blocking=jitter=0. Calls index 0 and 1. No explicit result assertion. |
| response_bounds.rs:81 a_lone_task_pays_only_for_itself | 3 | One admitted task; period positive and <100000; execution/blocking/jitter <100000; deadline=u64::MAX. If Bounded, assert `r == c+b+j`. |
| response_bounds.rs:100 an_index_past_the_end_is_named | none specified | Empty TaskSet, arbitrary usize index constrained >0; expects NoSuchTask. Does not exercise index 0 of an empty set. |

`Cargo.toml` lines 18–26 include `kani/**/*.rs` in the published archive, but the entire manifest has no `[[test]]` target or other target path pointing to the harness. `src/lib.rs` has no `mod` or `include!` binding to that file. The sole checked-in workflow `.github/workflows/ci.yml` runs Cargo formatting, Clippy, tests, a bare-metal build, docs, and repository audits, but no Kani command. Packaging a source file is distinct from wiring it as a build/proof target. No proof execution or successful proof logs were observed by this worker; "five harness definitions" is supported, "five proven properties" is not established here.

The header of `kani/response_bounds.rs` lines 12–14 discusses an unwind bound of four, while actual attributes are 5, 5, and 3. The recurrence uses `ITERATION_CAP = 10_000` at `src/lib.rs:105` and loop at 473. These are separate bounds. Do not extrapolate successful bounded harness runs, if later obtained, to every task set or arbitrary-size recurrence without checking unwinding assertions and the exact checked inputs.

## Reproduction and limitations

`oracle.json` documents its field schema, records SHA-256 for every Rust source file, and stores every definition and callsite with exact source line. Inventory used line-anchored declaration matching and direct reading, independently of ai-architect's parser or graph. A second token search checked all non-comment fn tokens; attribute counts and direct call counts reconciled; source excerpts were checked against disk. This is a corpus-specific lexical audit, not a general Rust parser. Runtime test success, coverage, Kani success, numerical correctness and hardware guarantees are not conclusions of this oracle.
