# End-result evaluation harness

Black-box benchmark that scores the ai-architect-mcp pipeline's agent-facing
tool answers against hand-labeled ground truth.

Full spec: `stages/stage-3b-v2.md` §2.

## Run

```bash
cargo run --release -p bench-end-result --bin bench_end_result -- --corpus rust-self
cargo run --release -p bench-end-result --bin bench_end_result -- --all   # note the -p
```

The runner rebuilds the server (`cargo build --release -p ai-architect-mcp-codebase`)
before measuring, and refuses to measure if that build fails: `cargo run -p
bench-end-result` compiles only the bench crate, so without this a stale
`target/release` binary would be scored silently (issue #397). `--binary <path>`
skips the build and measures exactly that file. A full `--all` run takes about
3 minutes and about 0.9 GB of RSS on top of the release build.

Output:

- **stdout**: full JSON summary (machine-consumable).
- **stderr**: human-readable per-corpus/per-query table + verdict.
- **`benches/runs/<unix_ts>.md`**: markdown archive.

Exit code is 0 iff the aggregate clears the production-grade target
(end_result_score ≥ 0.85 AND no language below the 0.75 floor). Non-zero
otherwise.

## Layout

```
benches/
├── harness/                 — the bench crate (workspace member)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs          — CLI
│       ├── runner.rs        — spawns the MCP binary, drives JSON-RPC
│       ├── scoring.rs       — 4 score types + weighted mean + floor gate
│       ├── queries.rs       — the 14 canonical queries
│       ├── corpora.rs       — corpus config + ground-truth loader
│       └── report.rs        — JSON / human / markdown renderers
├── corpora/
│   ├── <name>/corpus.toml   — name, language, path
│   └── <name>/ground_truth.json — hand labels
└── runs/                    — markdown archive of past runs
```

## Add a corpus

1. `mkdir benches/corpora/<name>` and `mkdir benches/corpora/<name>/src`
   (or add a git submodule pointing at a public repo).
2. Write `benches/corpora/<name>/corpus.toml`:

   ```toml
   name = "<name>"
   language = "<rust|typescript|python|kotlin|go|swift|java|javascript>"
   path = "./src"             # relative to the corpus dir, or absolute
   description = "..."
   # git_rev = "<sha>"        # optional: read `path` (repo-root-relative) from this commit
   ```
3. Write `benches/corpora/<name>/ground_truth.json` (see next section).
   An empty `labels: []` array is fine as a stub — the harness will skip
   the corpus and flag it as TODO.

## Add labels

Each label is one entry in `ground_truth.json`:

```json
{
  "query_id": "q4",
  "input":    { "qualified_name": "main.rs::handle_tool_call" },
  "expected": { "callers": ["main.rs::handle_request"] }
}
```

- `query_id` — one of `q1`..`q14` (see `src/queries.rs`).
- `input` — forwarded to the MCP tool as JSON arguments (the runner adds
  `graph_path` automatically).
- `expected` — ground truth; the scorer reads the field corresponding to
  the query's score type:

| Query | Expected field |
|---|---|
| q1, q2, q3, q10 | `qualified_name` (string — exact match) |
| q4 | `callers` (array of qn) |
| q5 | `callees` (array of qn) |
| q6 | `implementors` (array of qn) |
| q7 | `affected` (array of qn) |
| q8 | `symbols` (array of qn) |
| q9 | `imports` (array of qn) |
| q11 | `fields` (array of qn) |
| q12 | `partition` (array of `{qn, cluster}`) |
| q13 | `truly_present` (array of strings) |
| q14 | `unresolved` (array of strings) |

### Labeling rules

**Labels are ground truth — they describe the source code, not what the
pipeline output happens to be.** If the pipeline is wrong, the label is
still right, and the benchmark catches the pipeline's error. That is the
point of the benchmark.

Workflow:
1. Pick a symbol or relation in the corpus.
2. Open the source file. Read it.
3. Record what's **actually there** as the `expected` value.
4. Do not consult the pipeline's output while labeling.

For Q4 (callers) and Q5 (callees), grep the codebase for the symbol name
and inspect call sites by hand. For Q6 (implementors), grep for
`impl <Trait>` / `implements <Interface>` / `extends <Class>`. For Q12
(clustering), a coarse by-module partition (`parser/* → 0`, `search/* →
1`, etc.) is acceptable as a first pass.

## Status of shipped corpora

| Corpus | Labels | Notes |
|---|---|---|
| `rust-self` | 44 | Self-referential — our own `src/`. |
| `typescript-small` | 16 | Synthetic hand-authored TS. |
| `python-fastapi` | 0 | Stub — labels and source TODO. |
| `kotlin-coroutines` | 0 | Stub — Kotlin adapter not shipped. |
| `go-kubectl` | 0 | Stub — Go adapter not shipped. |

Target per §2.4 is ~200-300 labels per corpus across all 14 queries.
The initial pass ships enough to produce an honest baseline, not the full
label set.

## Deferred

- **LSP oracle tier** (§2.4): not implemented. Labels are hand-only today.
- **Q12 adjusted-Rand clustering labels**: need a hand-partitioned module
  map per corpus. Deferred for this pass.
- **Q13 PRD validation labels**: need PRD fixtures paired with the graph.
  Deferred.
- **Swift / JavaScript sentinel corpora**: stubs not yet created.

## Pinned corpora and label policy

The `rust-self` corpus is this repository's own `src/`. Its labels are
exhaustive (every top-level fn of a file, every caller of a function), so
indexing the live tree makes every merged PR a silent label regression: the
score fell from 0.906 to 0.752 without any regression in the tools (issue
#397). `corpus.toml` therefore sets `git_rev`: the harness extracts
`<git_rev>:<path>` with `git archive` into a temporary directory and indexes
that immutable tree. It needs the commit in the local object database, so a
shallow clone must `git fetch --unshallow` first (the error says so).

- q4/q7 callers are ALL callers, test functions included, including calls
  inside `assert!` token trees and via `use super::*`. The labels follow the
  source, not what the tool happens to see: measured on the pinned tree the
  tool resolves all of them (q4 = q7 = 1.000 on rust-self), so the policy keeps
  the labels honest if that ever regresses.
- q14 queries only the external crate roots the label lists.

Move the pin only deliberately: set `git_rev` to a commit on main, re-derive
every path-, caller-, import- and field-set label against that tree (source
scan first, then the tool; a disagreement is a finding, not a label), run the
bench, and record the new baseline in the PR. The stale-path guard (#132)
still fails the run (exit 3) if a label names a file absent from the pinned tree.
