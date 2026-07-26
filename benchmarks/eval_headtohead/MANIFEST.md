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
  (the corpus IS the pin — §AC5; recomputed at every run and printed).
- **Base commit:** `a4fa000` (branch `eval/issue-64-head-to-head`).
- **Date:** 2026-07-26.
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

| metric (mean ± stdev, n=20) | GRAPH | EXPLORER |
|---|---:|---:|
| retrieval precision | **1.00 ± 0.00** | 0.65 ± 0.33 |
| retrieval recall | 0.83 ± 0.34 | **1.00 ± 0.00** |
| tokens (est.) | **36.7 ± 19.8** | 550.4 ± 330.3 |
| tool calls | **1.0 ± 0.0** | 5.2 ± 1.6 |

- **Token ratio (explorer / graph): 17.4× mean.** The graph answer is bounded and
  paginated; the file-exploring transcript grows with the files grep surfaces.
- **Tool-call ratio: 5.2× mean.** One graph tool call vs glob + grep + N reads.

### Hypotheses (pre-registered thresholds)
| H | Claim | Verdict | Evidence |
|---|---|---|---|
| H1 | tokens: explorer/graph ratio > 1.5× | **SUPPORTED** | 17.4× |
| H2 | tool calls: ratio > 2× | **SUPPORTED** | 5.2× |
| H3 | GRAPH D2 precision ≥ EXPLORER + 0.15 | **SUPPORTED** | 1.00 vs 0.40 |
| H4 | GRAPH recall not > 0.10 below EXPLORER | **FALSIFIED** | 0.83 vs 1.00 |

## The honest negative (H4 FALSIFIED) — reported, not buried (§AC6)

The graph wins tokens, tool calls, and precision decisively, but its **recall is
0.17 below the baseline**, and the pre-registered guard H4 catches it. The
substring baseline finds every occurrence (recall 1.0 by construction); the graph
misses answers on four specific rows (see `raw_results.json`):

- **`go-D3` (recall 0):** AP does not classify a Go program entry as `kind=main`
  (all Go entries came back `composable_candidate`), so `get_processes` filtered
  to entry points returns nothing. `Grep "main"` finds `main.go`. Baseline wins.
- **`go-D4`, `ts-D4`, `rs-D4` (recall 0 / 0.5 / 0.5):** reverse type-usage edges
  (`Uses`/`Imports` → `get_impact` users/importers) are fully populated in
  **Python** (recall 1.0) but partial in TypeScript/Rust and absent for the Go
  struct. This is a measured limitation of cross-language type-usage resolution.
- **`rs-D2` (recall 0.5):** `worker.rs` calls `process_order` via a higher-order
  reference (`queue.iter().map(process_order)`), idiomatic Rust the call-graph
  resolver does not capture; `get_impact` finds only the direct caller.

None of these were engineered to make the graph lose — they are what AP's
resolver actually does on idiomatic code, surfaced by a pre-registered guard.
They are the honest counterweight to the token/precision wins, and they point at
concrete resolver work (Go entry classification, cross-language `Uses` edges,
higher-order call edges). Per-language and per-dimension breakdowns are in
`results.json`; every per-question row, including these losses, is in
`raw_results.json`.

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
