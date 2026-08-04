# Pre-registration — Graph tools vs Grep/Glob/Read baseline (issue #64)

**Status: PRE-REGISTERED. Written and committed BEFORE the harness was executed.**
A plan written after seeing results is not a pre-registration (§3.2, Fisher-style
design). This document fixes the hypotheses, conditions, question set, metrics,
grading rubric, scope and stopping rule up front. The result set
(`results.json`, `raw_results.json`) is produced by running the committed harness
against the committed corpus; nothing in this plan is edited in response to the
numbers.

- **Author:** experiment-runner (Claude), for cdeust/ai-architect-mcp-codebase.
- **Registration date:** 2026-07-26.
- **Parity tracker:** cdeust/enterprise-backlog#26. Reference design:
  `DeusData/codebase-memory-mcp` `docs/EVALUATION_PLAN.md` (peer-review-before-
  execution, blinded LLM-judge, Graph vs Explorer). Their plan is explicitly
  **unexecuted**; ours is designed to actually run and publish numbers — that is
  the differentiator (see `MANIFEST.md`).

---

## 1. Question under test

The product's core proposition is that answering a structural code question with
a **graph query** beats **file-by-file exploration** (`Grep`/`Glob`/`Read`) on
tokens consumed and on answer quality, at equal or better retrieval correctness.
Until now that has been asserted, not measured (§8 unsourced-claim; §9 "it works"
is not a source). This evaluation measures it.

## 2. Conditions (two, same questions, same corpus)

- **GRAPH** — the AP graph tools only. Each question is answered by exactly one
  (occasionally two) library call on the SAME code paths the MCP tools run:
  `search::search_graph` (search_codebase), `clustering::get_impact`
  (get_impact), `clustering::get_processes` (get_processes), and
  `GraphStore::execute_query` (query_graph). No file reads.

- **EXPLORER** — the baseline: `Grep`/`Glob`/`Read` only, as a backgrounded
  sub-agent would use them. Operationalized deterministically and **competently**
  (a weak baseline is a rigged result — AC2):
  - `Glob` the corpus for candidate source files (one call).
  - `Grep` each question keyword as a **case-insensitive substring** (`rg -i`) —
    the setting a competent agent uses for discovery across camelCase / snake_case
    conventions, and the baseline's *most favourable* recall setting. Substring is
    applied **uniformly across all dimensions** so no keyword is hand-tuned per
    question; multiple keywords are unioned. The transcript is `path:lineno:line`
    for every match.
  - `Read` the full contents of every distinct file that had a keyword hit
    (deduped across the question's keywords) — the agent reads to confirm.
  - **Explorer retrieved-file set** = the set of files with a substring hit. This
    is exactly `rg <kw> -l`. We do NOT hand the baseline a magic post-read
    filter: the filtering judgment a human would apply after reading is precisely
    the "answer quality" the blinded judge assesses (§5). The deterministic
    retrieval metric therefore credits the graph's structural precision and
    grep's lexical recall **equally against ground truth, with no hand-tuning of
    either side.** Substring is the baseline's *most favourable* recall setting
    (it finds every occurrence); its precision cost on distractors is the real,
    measured property under test.

## 3. Corpus and scope (fixed BEFORE execution — §15.1)

**Scope decision, stated before the run (§15.1: if the budget forces a smaller
set, the set shrinks and the plan says so before execution, not the run stopping
early):** the corpus is a **committed, purpose-authored multi-language corpus**
vendored in `corpus/`, NOT cloned third-party repositories. Rationale, on the
record:

1. **No-network reproducibility.** A third party reruns `reproduce.sh` and gets
   the same numbers with no clone step, no rate limits, no upstream drift
   (§AC5 "runnable by a third party", "pinned … commits"). The corpus IS the
   pin: its content hash is recorded in `MANIFEST.md`.
2. **Licensing.** Vendoring third-party source into this MIT repo is avoided;
   the corpus is original code authored for this eval.
3. **Ground-truth precision.** Because we author the corpus, the correct answer
   set for every question is known exactly (the P/R-1.0 ground-truth pattern of
   issue #58), rather than hand-labelled on unfamiliar upstream code.

**Non-goal (AC/Non-goals):** matching CBM's 159-language scope. AP supports 10
languages; this corpus covers **4** (Python, TypeScript, Go, Rust) with a
**symmetric structure** so the 5 capability dimensions aggregate across
languages. The write-up says so plainly. The corpus deliberately contains
**realistic lexical distractors** — comments that mention a symbol name, string
literals, and substring-colliding names (`preprocess_order`, `process_orders`) —
because real code contains them; a grep baseline's precision loss on distractors
is a **real property of lexical search, not an artifact** introduced to rig the
result.

## 4. Capability dimensions (D1–D5) and question design

Each language contributes one question per dimension (5 × 4 = 20 questions), so
heterogeneous languages stay aggregatable (the CBM design). Dimensions:

| Dim | Name | Question shape | GRAPH tool | Ground truth |
|-----|------|----------------|-----------|--------------|
| D1 | Definition / API discovery | "Where is `X` defined?" | query_graph (exact name match) | the defining file |
| D2 | Reference / impact | "Which functions call `X`?" | get_impact | files with real call sites |
| D3 | Structure / processes | "What execution processes exist?" | get_processes | files holding process entry points |
| D4 | Dependency / usage | "What uses type/config `C`?" | get_impact (users/importers) | files that use `C` |
| D5 | Search / navigation | "Find symbols matching `K`" | search_codebase | files defining matching symbols |

Ground truth per question is a **set of files** (file-granularity retrieval,
identical scoring for both conditions — neither side gets symbol-level credit the
other cannot). GT is committed in `questions.json`.

## 5. Metrics (each with dispersion — AC4)

A point estimate without a spread is not a measurement (AC4). All three are
reported as **mean ± sample standard deviation** across questions, and broken
down per dimension and per language:

1. **Total tokens** — token proxy = characters / 4 (stated approximation; the
   *ratio* is proxy-independent), matching `benchmarks/token_surface`. GRAPH =
   compact serialized response chars; EXPLORER = grep transcript chars + full
   contents of every file read.
2. **Tool-call count** — GRAPH = library calls issued (1–2); EXPLORER = 1 glob +
   1 grep-union + one Read per distinct file read.
3. **Answer quality** — retrieval **precision / recall / F1** against ground
   truth (deterministic, offline, the #58 pattern) AND a **blinded
   LLM-as-a-Judge** score (0–4 rubric below). The judge leg is **config-gated**
   (see §7): it runs only when an API key is configured; its absence is reported
   loudly and never silently stubbed.

**Blinded LLM-judge rubric (0–4), graded against the actual source:**
- 4 — complete and correct: names every true answer, no false ones.
- 3 — correct but with ≤1 spurious or ≤1 missing item.
- 2 — partially correct: the majority of true items, some noise.
- 1 — mostly wrong: a minority of true items or dominated by noise.
- 0 — wrong or empty.

**Blinding protocol:** for each question the two answers are labelled `Answer A`
/ `Answer B` in a **seed-randomized order** (seed recorded), and the judge is
given only the question, the two anonymized answers, and the source — never which
condition produced which. The un-blinding map is written separately
(`raw_results.json`) and applied only after scores are recorded.

## 6. Hypotheses (falsifiable, directional)

- **H1 (tokens).** GRAPH consumes **fewer tokens** than EXPLORER, aggregate mean
  ratio EXPLORER/GRAPH **> 1.5×**. *Falsified if the ratio ≤ 1.5×.*
- **H2 (tool calls).** GRAPH issues **fewer tool calls** than EXPLORER, mean
  ratio **> 2×**. *Falsified if ≤ 2×.*
- **H3 (precision).** GRAPH's mean retrieval **precision ≥ EXPLORER's**, with a
  material margin (**≥ 0.15**) on the reference dimension D2 (impact), where
  lexical distractors bite. *Falsified if GRAPH precision < EXPLORER on D2.*
- **H4 (recall / no regression).** GRAPH's mean **recall is not worse** than
  EXPLORER's by more than 0.10 aggregate. *Falsified if GRAPH recall is >0.10
  below EXPLORER.* (This is the honest guard: the graph must not win tokens by
  silently missing answers.)

Any hypothesis that fails is reported **in the results table as a negative**, not
dropped (AC6; house precedent Cortex#170 shipped a negative as opt-in). A sweep
that reports only wins is not evidence.

## 7. Judge leg — runnable vs budget-gated split

The deterministic legs (tokens, tool calls, precision/recall/F1 vs ground truth)
run fully **offline** and produce the published numbers. The LLM-judge leg needs
an external model:

- It is **config-gated** by env: `AP_EVAL_JUDGE_CMD` (a shell command that reads a
  JSON judging request on stdin and writes `{"score": N}` on stdout — model-
  agnostic, no vendor lock). When set, the harness runs the blinded judge over
  every question and records scores + dispersion.
- When **unset**, the harness records `judge_status:
  "SKIPPED_BUDGET_GATED"`, prints a loud stderr banner, and exits successfully.
  The judge module, its prompt, the blinding, and a unit test with a **mock
  judge** are committed regardless — the leg is a documented, testable, config-
  gated component whose absence is loudly reported, never a silent stub (parent
  directive).
- **Cost bound (AC5), stated up front:** ≤ 20 questions × 1 judging call =
  **≤ 20 model calls per full run**, ≤ ~2k input tokens each ⇒ **≤ ~40k tokens**
  total. No sweep amplification; one call per question, one pass.

## 8. Reproducibility (AC5)

- Corpus content hash, toolchain, dependency set, hardware, date, RNG seed, and
  the exact command are recorded in `MANIFEST.md`.
- `reproduce.sh` runs the whole thing from a clean checkout.
- Seeds: the blinding RNG seed is **fixed at 64** (the issue number) and
  recorded; the deterministic legs have no randomness.

## 9. Stopping rule (AC1)

Fixed sample: **all 20 questions** (5 dimensions × 4 languages) are run in **one
pass**. No optional stopping, no per-question peeking to decide whether to
continue (§Move 1). The run does not stop early on a good-looking prefix, and it
does not add questions after seeing results (that would be HARKing). If a future
run extends the corpus, it is a NEW pre-registration.

## 10. Analysis plan

- Aggregate and per-dimension/per-language mean ± stdev for every metric.
- Ratios EXPLORER/GRAPH for tokens and tool calls.
- Each hypothesis H1–H4 marked SUPPORTED / FALSIFIED against its threshold.
- Every per-question row (including the ones GRAPH loses) published in
  `raw_results.json` (AC6).
- Every claim that reaches the README traces to a specific row/field (AC7).
