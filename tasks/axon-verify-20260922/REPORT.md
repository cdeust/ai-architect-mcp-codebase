# Close-out measurement for #282, #283, #284 on DY-WCET — 2026-09-22

The 2026-09-08 measurement (`tasks/axon-rerun-20260908/`), repeated with its own harness
against a release binary built from `origin/main` after fix lots 1–6 were merged. Same
frozen specimen, same oracle, same `semantic_sha256` definition. Every number below
labelled "2026-09-22" comes from a JSON-RPC transcript under `raw/`. Every number labelled
"2026-09-08" is quoted from that pass's `scores*.json` and `raw/*/analyze.json`.

## Identities

| Item | This pass (2026-09-22) | Baseline (2026-09-08) |
|---|---|---|
| Source commit | `f3b21573fd4711b27c1f02e5cb0e0230cb666fff` (`origin/main` tip; includes #298, #302, #303 and the tree-sitter 0.27.0 / lbug 0.20.4 / zstd 0.14.0 / tantivy bumps #306–#313) | `112d3efd2a4270a1352f7f0dcd676a5abfaa284d` |
| Binary sha256 | `c9b76bdc46ac05756098644a7f7794f4bae642aaac41ec874342f724244ebe96` | `f7513be8f83624c72bac37d910b52b07821821a40bf3fefc8156d6a42a4ed6ca` |
| Corpus | DYResearch/dy-wcet @ `1e93ccd462af2b2531c32e4ac9eae5bd8e95fb2b`, four source sha256 match `oracle.json` (checked by `measure.py` on every run) | same |
| rust-analyzer | 1.95.0 (59807616 2026-04-14) | 1.95.0 |

## Summary

| Measurement | 2026-09-08 | 2026-09-22 |
|---|---:|---:|
| **#282** nested under `[workspace] members = []`: `lsp_status.state` | `completed` | **`failed`** |
| **#282** nested: `lsp_status.error` | none | `lsp_workspace_load_failed: Failed to load workspaces.; …` |
| **#282** nested: `lsp_resolve` | `resolved 0 / failed 634` | `null` (no request issued) |
| **#282** nested: standalone `lsp_resolve` tool `reason` | not measured | `lsp_workspace_load_failed` |
| **#282** standalone: `lsp_status.state` / `server_health.health` | `completed` / field absent | `completed` / `ok` |
| **#283** static callsites resolved (`response_of`) | 0 / 52 | **44 / 52** |
| **#283** static distinct callers | 0 / 38 | **35 / 38** |
| **#283** static production callers | 0 / 4 | **4 / 4** |
| **#283** static Kani-harness callers | 0 / 4 | **4 / 4** |
| **#283** static false callers | 0 | 0 |
| **#284** `query_graph(graph="missed")` `outside_build_targets.files` | bucket absent | `["kani/response_bounds.rs"]` |
| **#284** LSP `outside_targets_count` | field absent | 44 |
| LSP (standalone) callsites resolved | 47 / 52 | **52 / 52** |
| LSP (standalone) distinct callers | 34 / 38 | **38 / 38** |
| LSP (standalone) false callers | 0 | 0 |
| LSP (standalone) `semantic_sha256` | `c0959f32dbe39d23…` | **`5d031f4f22004825…`** (moved; 3/3 replicates identical) |

Definition extraction is unchanged in every variant: 89/89 explicit functions, 49/49 test
entries, 5/5 proof entries, 52/52 `response_of` callsites extracted, zero extra.

## #282 — LSP pass under a parent Cargo workspace

**Input.** `analyze_codebase(path=<nested>/dy-wcet, language=rust, dependency_scope=none,
lsp=true)`, where `<nested>/Cargo.toml` is `[workspace]\nmembers = []` and `<nested>/dy-wcet`
is a byte copy of the oracle-checked corpus (`raw/lsp-nested`). On the same graph, the
standalone `lsp_resolve(graph_path, codebase_path=<nested>/dy-wcet, language=rust)`. The
control is the same corpus with no parent manifest (`raw/lsp-outside-1..3`).
`cargo metadata --no-deps` in the nested child exits 101 with `current package believes
it's in a workspace when it's not`, the same shape the issue reports.

**Does.** Before any `textDocument/definition` request, the pass reads the health that
rust-analyzer reports in `experimental/serverStatus`. When that health is `error`, the LSP
phase fails and names the cargo-level fix. Static resolution still runs and its graph is
kept.

**Returns.**

| Field | Nested (`raw/lsp-nested/analyze.json`) | Standalone (`raw/lsp-outside-1/analyze.json`) |
|---|---|---|
| `status` | `ok` | `ok` |
| `lsp_status.state` | `failed` | `completed` |
| `lsp_status.error` | `lsp_workspace_load_failed: Failed to load workspaces.; the language server loaded no crate graph, so every definition request would answer []. Fix: add the package to the parent workspace's members, or analyze the workspace root` | — |
| `lsp_status.fallback` | `available_graph` | — |
| `lsp_status.server_health` | — | `{health: ok, message: null, readiness: server_reported_quiescent}` |
| `lsp_resolve` | `null` | `resolved 88, failed 329, skipped 0, outside_targets 44` |
| `resolve.phase` | `static` | `static` |

The standalone tool on the nested graph (`raw/lsp-nested/lsp-resolve-tool.json`) returns
`{status: error, reason: lsp_workspace_load_failed, message: <same text>}`.

The nested graph hashes to `3acd91e7bbd35205…`, the same value as the static-only pass
(`raw/static`). The failed LSP phase added no edges, and the 35/38 callers reported for
the nested run all come from static resolution.

**Establishes.** Under the #282 shape, the tool now reports that the LSP phase failed and
why, both from `analyze_codebase` and from `lsp_resolve`. It no longer returns
`completed` with zero resolution. In standalone the state stays `completed`, and the server
health is visible as `ok`.

**Probe E (analysing the parent root), measured and not as forecast.** Plan §8 #282(4)
expected `completed_unresolved` with `health: warning`. The run analysed
`<nested-nogit>/` with `<nested-nogit>/Cargo.toml` = `[workspace] members = []` and a
`.git`-less byte copy of the corpus under `dy-wcet/` (`raw/lsp-parent-root-nogit`). It
returned:
`lsp_status = {state: completed, server_health: {health: warning, message: "Failed to read
Cargo metadata … The manifest is virtual, and the workspace has no members.", readiness:
server_reported_quiescent}}`, `lsp_resolve = {resolved 0, failed 0, skipped 0,
outside_targets 461}`. `coverage.outside_build_targets.files` lists all four `.rs` files.
Finding F1 below covers this. When the child keeps its own `.git`
(`raw/lsp-parent-root`), the walker prunes it as `nested_repository`, `files_indexed = 0`,
and says so in `coverage.pruned_dirs`, so that shape cannot test the LSP path.

## #283 — static resolution of receiver method calls

**Input.** `analyze_codebase(..., lsp=false)` on the standalone corpus (`raw/static`), then
`get_impact("src/lib.rs::TaskSet::response_of")`. A census of the `response_of` in-edges by
`resolution_method` comes from `query_graph` (`response-edges-*`).

**Does.** For Rust, a receiver call binds statically in two cases. A `self.m()` / `Self::m()`
call binds to the enclosing `impl` type's method (`receiver-type`, confidence 0.93). A call
on a local bound exactly once by a typed parameter, `let x: T`, or `let x = T::…(…)` binds
to `T::m` (`receiver-local-binding`, confidence 0.87). There is no fallback to name-only
binding.

**Returns** (`scores.json`).

| | 2026-09-08 | 2026-09-22 |
|---|---:|---:|
| Callsites resolved | 0 / 52 | 44 / 52 (precision 1.0) |
| Distinct callers | 0 / 38 | 35 / 38 (precision 1.0, fp 0) |
| Production callers | 0 / 4 | 4 / 4 |
| Kani-harness callers (`kani/response_bounds.rs`) | 0 / 4 | 4 / 4 |
| `resolve.resolution_rate` / `total_edges` (whole graph) | 0.39 / 340 | 0.57 / 494 |
| `get_impact.unresolved_callsites_naming_target` | field absent (reason text: 52) | 8 |

Edges by method: `receiver-type` ×3, all in `src/lib.rs`: `is_schedulable`,
`first_failure`, `slack_of`. `receiver-local-binding` ×32: `src/lib.rs` 14 (this includes
`optimal_priority_order` via `trial`), `tests/on_paper.rs` 12, `kani/response_bounds.rs` 4,
`tests/properties.rs` 2.

The 3 missing callers and 8 unresolved sites are all in `tests/properties.rs` (lines 66, 98,
153, 155, 168, 171, 179, 182). The functions are
`a_response_is_never_shorter_than_the_work_it_contains`,
`the_same_set_gives_the_same_bits_across_repetition`, and
`a_bounded_answer_always_meets_its_deadline_and_an_unbounded_one_never_does`. Their receivers
come from `let s = generate(&mut rng, n);`. A free-function initializer carries no type
name, so the resolver attaches no binding (plan §2.2) and leaves these sites open.
`get_impact` reports `epistemic: lower-bound` with the reason "8 unresolved call site(s)
name this symbol…".

**Establishes.** The static pass alone now finds every production caller and every Kani
harness caller of `TaskSet::response_of`. It adds no false caller. The 2026-09-08 static
figure was 0/38. The forecast in plan §7 ("~38/38") is not reached: 35/38 is what the
single-binding rule reaches on this corpus. The plan's forecast of 4/4 for the Kani
harnesses is met.

## #284 — calls from files outside the compiled Cargo targets

**Input.** `query_graph(graph="missed")` and `index_status` on every graph.
`analyze_codebase(lsp=true)` standalone. A `CallSite` census of `kani/response_bounds.rs`
(`raw/lsp-outside-1-followup/kani-callsites.json`). `get_impact` on two symbols that
unresolved Kani call sites still name, `src/lib.rs::Response::meets` and
`src/lib.rs::Task::jitter`.

**Does.** The tool derives the set of compiled files from `cargo metadata --no-deps
--offline`. It flags indexed `.rs` files that sit outside every target, sends no LSP request
for their call sites, and tags those sites `unresolved_reason = outside_compiled_targets`.
It counts them apart from `failed_count` and names the file in `get_impact`.

**Returns.**

- `coverage.outside_build_targets` = `{count: 1, files: ["kani/response_bounds.rs"]}` in
  `raw/static`, `raw/lsp-outside-1..3` and `index_status`
  (`raw/lsp-outside-1-followup/index-status.json`). In `raw/lsp-nested` it is
  `{count: 0, files: []}` (see F2).
- LSP standalone: `outside_targets_count = 44`, `failed_count = 329`, `resolved_count = 88`,
  the same in all three replicates. The 2026-09-08 figures were 406 failed and 228 resolved.
- `kani/response_bounds.rs` holds 67 call sites. 23 resolve statically (4 of them
  `response_of`). The other 44 are unresolved with `unresolved_reason =
  outside_compiled_targets`: `kani::any` ×16, `kani::assume` ×8, `assert!` ×7, and 13
  chained or parenthesised receivers such as `(Unbounded::Overflow).meets` and
  `s.push(task(…)).is_ok`. The 44 therefore matches `outside_targets_count` exactly.
- `get_impact("src/lib.rs::Response::meets")`: `unresolved_callsites_naming_target = 4`,
  `unresolved_callsites_outside_targets = 4`. Reason: "…4 of them sit in files outside the
  compiled Cargo targets (kani/response_bounds.rs) — unresolvable by the language server;
  static receiver binding may still resolve them". `get_impact("src/lib.rs::Task::jitter")`
  gives naming 5 and outside 1, with the same reason naming the file.
- For `response_of`, every Kani site now resolves statically. So
  `unresolved_callsites_outside_targets = 0`, and the attribution text does not appear on
  that symbol. Plan §8 #284(2)/(3) predicted 5 against the pre-lot-6 state; after lot 6 the
  measured value is 0 for `response_of` and 44 for the whole file.

**Establishes.** The tool now names the harness file as outside the build in coverage and
`index_status`. The LSP pass does not query that file and counts its sites apart from real
negative answers. `get_impact` attributes unresolved sites to the file by name. The
`response_of` gap that #284 described (4 harness callers missing) is closed by static
binding, not by the language server.

## LSP pass, corpus outside any workspace (the `scores-outside.json` equivalent)

**Input.** `measure.py --lsp` three times on the standalone copy (`raw/lsp-outside-1..3`).

**Returns** (`scores-outside.json`). Callsites resolved: 52/52. Distinct callers: 38/38.
Production callers: 4/4. Kani callers: 4/4. False callers: 0. Edges by method:
`receiver-type` 3, `receiver-local-binding` 32, `lsp-definition` 3. The three
`lsp-definition` edges are exactly the three `generate`-bound callers in
`tests/properties.rs` that the static pass leaves open. `get_impact` reports
`epistemic: lower-bound` with a single reason: "38 reverse-dependency edge(s) were resolved
heuristically (confidence < 1.0)…". `semantic_sha256` is
`5d031f4f22004825fda11ab8e2e2f5e114cf65283b4d49eacfc029e8771899fb` in all three replicates.

**Why the hash moved from `c0959f32db…`.** The hash covers definitions, every
`Calls_*_*` edge, the 52 `response_of` callsite rows, and the impact callers. Running the
2026-09-08 `score.py` unchanged on `raw/lsp-outside-1` gives the same `5d031f4f…`, so the
function is the same and the graph is different. The 2026-09-08 `raw/lsp-tmp1/semantic.json`
and this run's `raw/lsp-outside-1/semantic.json` differ as follows.

| Component | 2026-09-08 | 2026-09-22 | Removed | Added |
|---|---:|---:|---:|---:|
| Definitions | 89 | 89 | 0 | 0 |
| `response_of` callsite rows | 52 | 52 | 5 | 5 (the 5 Kani sites flip `is_resolved` false→true) |
| Impact callers | 34 | 38 | 0 | 4 (the 4 Kani harnesses) |
| Call edges | 269 | 277 | 7 | 15 |

Each of the 15 added edges was checked against the source line. All 15 are real calls:
Kani harness → `response_of` ×4 and → `push` ×3, `Response::bound` ×3, `Response::meets` ×2,
`Response::is_bounded` ×2, `Task::deadline` ×1. The 7 removed edges fall into two groups
(F3):

- 4 were false. They were `→ src/lib.rs::tests::t` from `TaskSet::push`,
  `kani::task`, and two Kani harnesses, none of which calls `t(`.
- 3 were real calls that are now missing:
  `tests/properties.rs::generate → Task::deadline / Task::jitter / Task::blocking`.

The hash moved, and a move was expected: the fix changes resolution. It also moved for a
reason the fixes do not explain, the three lost true edges.

## Findings left unfixed

**F1: `state: completed` when every site is outside targets.** Analysing a virtual
workspace root whose `members` does not include the crate returns `lsp_status.state =
completed` with `resolved 0, failed 0, skipped 0, outside_targets 461`. Each individual
signal is present (`server_health.health = warning` with the cargo message, all four files in
`outside_build_targets`, `outside_targets_count = 461`). A caller that reads only `state`
still sees the green answer #282 set out to remove. This follows from the lot-5 rule that
`outside_targets` does not feed `completed_unresolved` (`src/analyze_handlers/lsp_outcome.rs`).
Plan §8 #282(4) forecast `completed_unresolved`. Raw: `raw/lsp-parent-root-nogit/analyze.json`.

**F2: an unknown target map looks like a clean one.** In the #282 nested shape, `cargo
metadata` fails, so the Cargo target map is unknown. By design (plan §3.2) no file is then
attributed. `coverage.outside_build_targets` renders as `{count: 0, files: []}`, which is
identical to "every file is compiled", although `kani/response_bounds.rs` is outside the
build there too. With `lsp=true` the same response carries `lsp_status.state = failed`. A
static-only analysis would carry no signal: it was not run in this shape, so this is
inferred from the same code path, not measured. Raw: `raw/lsp-nested/coverage.json`.

**F3: three true call edges lost since 2026-09-08, not explained by #282–#284.** In both the
static and the LSP pass, `tests/properties.rs::generate` no longer has edges to
`Task::deadline`, `Task::jitter`, or `Task::blocking`. The source is
`Task::new(wcet, period).deadline(deadline).jitter(jitter).blocking(blocking)`, lines 48–51.
The 2026-09-08 graph had three extra bare-name `CallSite`s (`deadline`@49, `jitter`@50,
`blocking`@51) that `unique-match` resolved. They are absent now. The full-chain sites that
remain are unresolved in both passes, then and now. `CallSite` count went from 825 to 806
(−19). 7 of the 19 are accounted for: those 3 sites, plus 4 bare `t` sites whose removal took
the 4 false `→ tests::t` edges with it. The other 12 are not characterised. This pass did not
bisect which commit between `112d3ef` and `f3b2157` changed call-site extraction; the
tree-sitter 0.26.11 → 0.27.0 bump (#306) is one candidate, and it is untested. Raw:
`raw/baseline-diff/`.

**F4: `get_impact.next_steps` has no pointer for outside-target sites.** Plan §3.3 (lot 5)
specified a `next_steps` entry pointing at `query_graph(graph="missed").outside_build_targets`.
The measured envelope carries the structured count and the reason text, but `next_steps` has
no such entry (`raw/lsp-outside-1-followup/impact-src_lib.rs__Response__meets.json`), and
`src/process_impact_handlers/impact.rs` references outside targets only in the envelope
field.

## Timing observation (not a verdict)

`analyze_codebase` wall time with LSP was 25.5–27.2 s here, against 11.9–14.4 s on
2026-09-08. The in-server `lsp_resolve.elapsed_ms` varied from 6 705 to 21 872 ms across
three identical replicates. The host was shared with concurrent builds and test runs. The
numbers are recorded, and no conclusion is drawn from them.

## Reproduce

Build (worktree at `.claude/worktrees/lot7-verify`, `HEAD == origin/main ==
f3b21573fd4711b27c1f02e5cb0e0230cb666fff`):

```sh
cargo build --release
#    Compiling ai-architect-mcp-codebase v0.11.1 (…/.claude/worktrees/lot7-verify)
#     Finished `release` profile [optimized] target(s) in 4m 19s
shasum -a 256 target/release/ai-architect-mcp-codebase
# c9b76bdc46ac05756098644a7f7794f4bae642aaac41ec874342f724244ebe96
```

The build log was captured as its last 40 lines. They contain no `warning` line, and the
crate's own `Compiling` line is followed directly by `Finished`. Toolchain: rustc/cargo
1.95.0, Python 3.14.4.

Specimens (outside any Cargo workspace, since this repository is one):

```sh
S=<scratch>/specimens
cp -R tasks/axon-rerun-20260908/corpus-git $S/standalone                    # git HEAD 1e93ccd…
mkdir -p $S/nested && cp -R tasks/axon-rerun-20260908/corpus-git $S/nested/dy-wcet
printf '[workspace]\nmembers = []\n' > $S/nested/Cargo.toml
mkdir -p $S/nested-nogit && cp -R $S/standalone $S/nested-nogit/dy-wcet && rm -rf $S/nested-nogit/dy-wcet/.git
printf '[workspace]\nmembers = []\n' > $S/nested-nogit/Cargo.toml
```

The standalone copy gained `Cargo.lock` and `target/` from rust-analyzer's flycheck during
the LSP runs. The walker prunes `target/` (`coverage.pruned_dirs`), and the four oracle
sources were re-verified by `measure.py` before every run.

Runs, in the order executed:

```sh
B=<worktree>/target/release/ai-architect-mcp-codebase
python3 measure.py --binary $B --corpus $S/standalone --output raw/static
python3 measure.py --binary $B --corpus $S/standalone --output raw/lsp-outside-{1,2,3} --lsp   # x3
python3 measure.py --binary $B --corpus $S/nested/dy-wcet --output raw/lsp-nested --lsp --lsp-tool
python3 measure.py --binary $B --corpus $S/nested/dy-wcet --analyze-path $S/nested --output raw/lsp-parent-root --lsp
python3 measure.py --binary $B --corpus $S/standalone --analyze-path $S/nested-nogit --output raw/lsp-parent-root-nogit --lsp
python3 followup.py $B raw/lsp-outside-1/graph raw/lsp-outside-1-followup \
        'src/lib.rs::Task::jitter' 'src/lib.rs::Response::meets'
python3 score.py --oracle oracle.json --output scores.json raw/static
python3 score.py --oracle oracle.json --output scores-outside.json raw/lsp-outside-{1,2,3}
python3 score.py --oracle oracle.json --output scores-nested.json raw/lsp-nested
```

The two `lsp-parent-root*` runs stop at `get_impact`, because qualified names there carry
the `dy-wcet/` prefix and the oracle target does not exist (the error suggests the prefixed
name). Only their `analyze`, `status` and `coverage` transcripts are used. For
`raw/baseline-diff/`, `edge_probe.py` and `callsite_probe.py` ran the same binary read-only
against a scratch copy of `tasks/axon-rerun-20260908/raw/lsp-tmp1/graph` (`*-old`) and
against `raw/lsp-outside-1/graph` (`*-new`).

`measure.py` and `score.py` are the 2026-09-08 scripts with additive changes only, listed in
their docstrings. The unmodified 2026-09-08 `score.py` reproduces `3acd91e7…` (static and
nested) and `5d031f4f…` (LSP) on this pass's raw directories. `oracle.json` and
`oracle-notes.md` are copied unchanged. `raw/` keeps the JSON-RPC request/response
transcripts. The graph database, search index and server state files are not archived.
