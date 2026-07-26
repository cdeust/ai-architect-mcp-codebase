# eval_headtohead benchmark — MANIFEST

The falsifiable head-to-head evidence for issue #64: **AP graph tools vs a
`Grep`/`Glob`/`Read` baseline**, on the same questions over the same committed
multi-language corpus. Pre-registered in `PRE_REGISTRATION.md` (written and
committed **before** this ran); the design, hypotheses, metrics, and stopping
rule were fixed in advance. Unlike the reference design it parries
(`DeusData/codebase-memory-mcp` `docs/EVALUATION_PLAN.md`, explicitly
**unexecuted**), this one **actually runs and publishes numbers** — including the
questions AP loses.

Reproducible: the corpus is committed and content-hashed, so
`cargo run -p eval-headtohead-bench --release` (or `./reproduce.sh`) regenerates
`results.json` + `raw_results.json` byte-for-byte on the same toolchain.

## Provenance
- **Corpus SHA-256:** `ac80fa254b8524afba418143183278d95b081eee838b03695a6efdc635902024`
  (the corpus IS the pin — §AC5; recomputed at every run and printed). Unchanged
  by the #87 fix — the corpus bytes are identical; only the resolver/extractor
  changed.
- **Base commit:** `a4fa000` (branch `eval/issue-64-head-to-head`) for the
  original falsifying run; re-run on branch `fix/recall-gaps-87` (issue #87)
  after the go-D3 / rs-D2 fixes.
- **Date:** 2026-07-26 (original run) · 2026-07-26 (#87 re-run).
- **Original falsifying run preserved at:** `results.2026-07-26-pre-fix-87.json`
  + `raw_results.2026-07-26-pre-fix-87.json` (the H4-FALSIFIED record stays on
  file; `PRE_REGISTRATION.md` is untouched). The canonical `results.json` /
  `raw_results.json` now hold the #87 re-run, so `reproduce.sh` regenerates them
  byte-for-byte on the current tree.
- **Hardware:** Apple Silicon (`arm64` / `aarch64`), macOS 26.5.1.
- **Toolchain:** rustc 1.95.0 (59807616e 2026-04-14), release profile.
- **Search/graph dependency set:** tantivy 0.26, lbug 0.18 (issue #78 set). A
  search-engine change can move ranking; these numbers were regenerated on this
  set, not carried over.
- **Blinding RNG seed:** 64 (fixed, recorded). The deterministic legs have no
  randomness; the only seeded choice is the judge's A/B presentation order.
- **Command:** `cargo run -p eval-headtohead-bench --release`.
- **Answer-quality judge:** config-gated (`AP_EVAL_JUDGE_CMD`). The numbers below
  are from the **offline** run with the judge **budget-gated OFF** (see §7 of the
  pre-registration and "Judge leg" below).
- **Cost bound (stated up front, §AC5):** the deterministic legs cost $0 (no API).
  The judge leg, when enabled, is **≤ 20 model calls per run** (one per
  question), ≤ ~40k tokens total — no sweep amplification.

## Conditions (two, same questions, same corpus)
- **GRAPH** — one AP tool per question on the real code paths
  (`search_graph`, `get_impact`, `get_processes`, `execute_query`). No file reads.
- **EXPLORER** — the `Grep`/`Glob`/`Read` baseline, run competently: `Glob` (1),
  a **case-insensitive substring** `Grep` (`rg -i`, the baseline's most
  favourable recall setting; 1 unioned call), then `Read` the full contents of
  every file with a hit. Retrieved set = `rg -i <kw> -l`. No magic post-read
  filter — that judgement is what the blinded judge assesses (§5 of the plan).

The corpus (`corpus/{python,typescript,go,rust}`) is a symmetric order-pipeline
authored for this eval, with **realistic lexical distractors** — a comment and a
string that mention the target symbol, and substring-colliding names
(`preprocess_order`, `process_orders`). A grep baseline's precision loss on those
is a real property of lexical search, not an artifact.

## Metrics (each with dispersion — §AC4)
Token proxy = characters / 4 (stated approximation; the *ratio* is
proxy-independent), matching `benchmarks/token_surface`. All numbers are mean ±
sample stdev across questions, also broken down per dimension and per language in
`results.json`.

## Headline results (offline run, judge gated off)

Two runs are on record. The **original** run FALSIFIED H4 (that is the eval doing
its job — a pre-registered guard catching a real product gap); the **#87 re-run**
is after go-D3 and rs-D2 were fixed. Both columns below quote GRAPH; EXPLORER is
unchanged between runs (its numbers do not depend on the resolver).

| metric (mean ± stdev, n=20) | GRAPH (original) | GRAPH (#87 re-run) | EXPLORER |
|---|---:|---:|---:|
| retrieval precision | 1.00 ± 0.00 | **1.00 ± 0.00** | 0.65 ± 0.33 |
| retrieval recall | 0.83 ± 0.34 | **0.90 ± 0.26** | **1.00 ± 0.00** |
| tokens (est.) | 36.7 ± 19.8 | **38.2 ± 19.0** | 550.4 ± 330.3 |
| tool calls | 1.0 ± 0.0 | **1.0 ± 0.0** | 5.2 ± 1.6 |

- **Recall improved 0.825 → 0.90** (mean over 20 questions): go-D3 0.0→1.0 and
  rs-D2 0.5→1.0. The remaining 0.10 gap is the type-usage gap deferred to #92.
- **Token ratio (explorer / graph): ~17× mean.** The graph answer is bounded and
  paginated; the file-exploring transcript grows with the files grep surfaces.
- **Tool-call ratio: 5.2× mean.** One graph tool call vs glob + grep + N reads.

### Hypotheses (pre-registered thresholds)
| H | Claim | Original | #87 re-run | Evidence |
|---|---|---|---|---|
| H1 | tokens: explorer/graph ratio > 1.5× | SUPPORTED | **SUPPORTED** | ~17× |
| H2 | tool calls: ratio > 2× | SUPPORTED | **SUPPORTED** | 5.2× |
| H3 | GRAPH D2 precision ≥ EXPLORER + 0.15 | SUPPORTED | **SUPPORTED** | 1.00 vs 0.40 |
| H4 | GRAPH recall not > 0.10 below EXPLORER | **FALSIFIED** (0.83 vs 1.00) | **SUPPORTED** (0.90 vs 1.00) | see below |

## The honest negative (H4) and its #87 disposition (§AC6)

The **original** run's honest negative — recall 0.17 below the baseline, caught by
the pre-registered H4 guard — is preserved in `raw_results.2026-07-26-pre-fix-87.json`
and stays the record; the eval exists to surface exactly this. #87 then closed two
of the three underlying gaps and classified the third out of scope (with a filed
issue, not a shrug):

- **`go-D3` (recall 0 → 1.0) — FIXED.** AP classified every Go function as
  `composable_candidate` and none as `kind=main` (Go is case-sensitive; the
  corpus entry is the exported `func Main`, which the lowercase-`main` rule
  missed), so `get_processes` filtered to entry points returned nothing. Fix:
  `clustering::process::detect_entry_points` now recognizes a Go `Main` as a
  program entry point (the idiomatic testable-entry convention: a thin `func main`
  delegating to `os.Exit(cli.Main())`), gated to `f.language = 'go'`. Regression:
  `benchmarks/eval_headtohead/tests/recall_gaps_87.rs::gap1_go_main_is_a_main_kind_entry_point`.
- **`rs-D2` (recall 0.5 → 1.0) — FIXED.** `worker.rs` references `process_order`
  via a higher-order argument (`queue.iter().map(process_order)`), idiomatic Rust
  the walker did not capture, so `get_impact` found only the direct caller. Fix:
  the Rust walker (`parser::rust::extract::g4`) now emits a CallSite for
  function-value arguments (bare identifier / path passed by value), which the
  resolver binds to the referenced function. Regression:
  `recall_gaps_87.rs::gap3_rust_higher_order_caller_is_captured` +
  `tests/parser_fidelity.rs::issue87_rust_higher_order_arg_is_captured_as_call_site`.
- **`go-D4`, `ts-D4`, `rs-D4` (recall 0 / 0.5 / 0.5) — OUT OF SCOPE → issue #92.**
  Reverse type-usage (`Uses_*`) edges are populated only from **Field** type
  annotations and from **calls** whose callee resolves to a type. Python's D4
  passes because it constructs via a plain call (`OrderConfig()`); Go/Rust/TS
  construct via composite/struct literals and `new`, and use the type in
  return-type annotations — none of which the graph captures today (Function/Method
  nodes carry no return-type/signature data; there is no Parameter node). Closing
  this requires **new type-reference extraction in the walkers** (return-type
  annotations + type-construction expressions) across the Go spec, Rust walker, and
  TS walker, plus a `Uses_*_Class` table for TS — a cross-cutting extraction
  feature that also intersects the in-flight #60 LangSpec migration. It is filed as
  **#92** with root cause and acceptance criteria, per #87's sanctioned
  out-of-scope disposition. This is a limitation of cross-language type-usage
  resolution, stated plainly, not engineered away.

Per-language and per-dimension breakdowns are in `results.json`; every per-question
row is in `raw_results.json` (#87 re-run) and `raw_results.2026-07-26-pre-fix-87.json`
(original falsification).

## Judge leg (answer quality) — runnable vs budget-gated
The blinded LLM-as-a-Judge leg (`src/judge.rs`) is **config-gated** by
`AP_EVAL_JUDGE_CMD` — a shell command that reads a JSON judging request on stdin
and writes `{"score_a":N,"score_b":M}` (0–4) on stdout, model-agnostic. When set,
each question's two answers are presented **blinded** (`answer_a`/`answer_b`, seed
64-randomized order; un-blinding recorded in `raw_results.json`) and graded 0–4
against the actual source. When unset — as in the published run — the leg is
SKIPPED, a loud banner is printed, and the deterministic retrieval
precision/recall/F1 vs ground truth stand as the offline answer-quality evidence.
The judge module, its prompt contract, the blinding, and a unit test with a mock
judge are committed regardless — the absence is loud, never a silent stub.

## Why this is a benchmark, not a CI gate
Token/tool-call ratios and the corpus content depend on the toolchain and
dependency set; a ratio is not a green-everywhere property (the `benchmarks/`
convention, and the issue #74 lesson — no wall-clock/ratio assertions in CI).
What CI *does* gate is the **correctness and determinism of the harness itself**:
`tests/eval_headtohead_harness.rs` asserts the known-answer precision/recall on
representative questions and that a re-run yields identical file sets. The
published ratios live here, with their provenance and their variance.
