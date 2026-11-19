# incremental_speed benchmark — MANIFEST

The performance evidence for incremental re-indexing (#62) and artifact
bootstrap-fill (#55): both are much cheaper than a full index. This claim was
**removed from the correctness test suite** in issue #74 — a wall-clock ratio is
not a deterministic, green-everywhere property — and lives here instead, with
recorded hardware and committed results (the `benchmarks/token_surface/` pattern).

Reproducible: the git fixture is generated in-code, so
`cargo run -p incremental-speed-bench --release` regenerates `results.json`.

## Provenance
- **Hardware:** Apple Silicon (`arm64` / `aarch64`), macOS 26.5.1.
- **Toolchain:** rustc 1.95.0 (59807616e 2026-04-14).
- **Date:** 2026-07-25.
- **Search/graph dependency set (issue #78):** tantivy 0.26.1, lbug 0.18.3.
  Re-run after the upgrade from tantivy 0.22.1 / lbug 0.15.4. A search-engine
  change can move ranking silently, so these numbers are not carried over from
  the previous versions; they were regenerated on this dependency set.
- **Command:** `cargo run -p incremental-speed-bench --release`.
- **Profile:** release (the headline `results.json`); debug numbers below for contrast.

## Methodology
- **Fixture:** a git repo of 260 symbol-dense Python modules (→ ~4.7k nodes,
  ~4.7k edges), then a 4-file diff (edit + add + delete + rename).
- **Real library:** drives `indexer::index_codebase_with_language`,
  `indexer::index_incremental`, `artifact::export_artifact` /
  `import_artifact`, and `indexer::fill_after_bootstrap` — the exact operations
  the tools run, not a re-implementation.
- **Best-of-N:** each operation is timed `ATTEMPTS=5` times and the **minimum**
  (least-contended, truest) is reported. The full index is re-run each attempt;
  the incremental copies the pristine graph each attempt; the fill re-imports the
  artifact into a fresh dir each attempt.

## Results (representative, otherwise-idle machine)
### Release
| operation | time | ratio to full |
|---|---:|---:|
| full index (260 files) | ~2500 ms | — |
| incremental (4-file diff) | ~280 ms | **~11%** |
| bootstrap-fill (4-file diff) | ~380 ms | **~15%** |

### Debug (same machine, for profile contrast)
| operation | time | ratio to full |
|---|---:|---:|
| full index | ~4500 ms | — |
| incremental | ~1100 ms | **~24%** |
| bootstrap-fill | ~1170 ms | **~26%** |

See `results.json` for the exact numbers of the last release run.

## Why this is a benchmark, not a correctness gate (issue #74)
A wall-clock RATIO is **hardware-, profile-, and load-dependent**, and its
variance exceeds any tight margin:

- The short op (incremental / fill) has a **fixed-cost floor** — a `git diff`,
  opening the DB, re-parsing a handful of files, and (for the fill) hashing the
  tree to write a local manifest. The full index instead **scales with repo
  size**. On faster hardware / the release profile the full index shrinks toward
  that floor, so the *ratio* can degrade even though both absolute times improve
  (the reporter of #74 saw a release `full` of 2037 ms give a 79% ratio).
- Observed spread on THIS machine at min-of-5, release: the fill ratio measured
  12%, 15%, and **62%** across three back-to-back runs (the 62% run was under
  concurrent load) — a >4× spread on identical inputs. A ≤30% gate below even the
  best observation on the reporter's machine could never be green everywhere.

That is a benchmark's job — report numbers with their hardware and their
variance — not a gate's. The correctness of #55/#62 (graph parity, exact change
partition, fill method) stays a hard gate in `tests/artifact_incremental_fill.rs`
and `tests/incremental_index.rs`, green on every machine and profile.

## Switch-over share (issue #443): `crossover`

`analyze_codebase` refreshes the previous graph when at most
`MAX_TOUCHED_PCT` (40) percent of the tree's files changed, and rebuilds
otherwise (`src/analyze_handlers/index_phase.rs`). The share comes from this
sweep: for each share of edited files (each gains a function), the base graph
is built as analyze leaves it (manifest, coverage sidecar, resolved edges),
then the refresh (`index_incremental` + `resolve_graph`) is timed against
the rebuild (manifest snapshot + `index_codebase_with_language` +
`resolve_graph`). Clustering and the search index run over the whole graph
on both paths and are left out.

- **Hardware:** Linux x86_64, 4 CPUs (cloud container), 15 GiB.
- **Toolchain:** rustc 1.95.0 (`rust-toolchain.toml`), lbug built from source
  (`LBUG_BUILD_FROM_SOURCE=1 ZSTD_SYS_USE_PKG_CONFIG=1`, as in CI).
- **Date:** 2026-11-16. **Profile:** release, min of 3.
- **Command:** `cargo run -p incremental-speed-bench --release --bin crossover -- <corpus>`.
- **Results:** `results_crossover.json`.

### `src/` (456 Rust files)
| edited | files | incremental + resolve | full + resolve |
|---:|---:|---:|---:|
| 1% | 5 | 5353 ms | 18129 ms |
| 5% | 23 | 7380 ms | 17725 ms |
| 10% | 46 | 8272 ms | 17031 ms |
| 20% | 92 | 12240 ms | 17315 ms |
| 30% | 137 | 14557 ms | 18285 ms |
| 40% | 183 | 18228 ms | 19444 ms |
| 50% | 228 | 22955 ms | 17845 ms |
| 75% | 342 | 29482 ms | 27401 ms |
| 100% | 456 | 52100 ms | 24432 ms |

The 75% and 100% rows ran while a cargo build used the same CPUs: they show the
trend, not a clean figure. The clean rows put the crossover between 40% and 50%.

### `src/parser/` (125 files)
| edited | files | incremental + resolve | full + resolve |
|---:|---:|---:|---:|
| 1% | 2 | 2120 ms | 4259 ms |
| 5% | 7 | 2593 ms | 4113 ms |
| 10% | 13 | 3797 ms | 4415 ms |
| 20% | 25 | 3199 ms | 4031 ms |
| 30% | 38 | 4947 ms | 4319 ms |
| 40% | 50 | 4617 ms | 4163 ms |
| 50% | 63 | 5762 ms | 3917 ms |
| 75% | 94 | 6590 ms | 4019 ms |
| 100% | 125 | 6939 ms | 4471 ms |

### `src/graph_store/` (24 files)
| edited | files | incremental + resolve | full + resolve |
|---:|---:|---:|---:|
| 1% | 1 | 1030 ms | 1272 ms |
| 5% | 2 | 1186 ms | 1227 ms |
| 10% | 3 | 1629 ms | 1179 ms |
| 20% | 5 | 1343 ms | 1158 ms |
| 30% | 8 | 1612 ms | 1186 ms |
| 40% | 10 | 1852 ms | 1243 ms |
| 50% | 12 | 2144 ms | 1241 ms |
| 75% | 18 | 2091 ms | 1215 ms |
| 100% | 24 | 1780 ms | 1122 ms |

The crossover moves down as the tree shrinks (between 40 and 50% at 456 files,
between 20 and 30% at 125, between 5 and 10% at 24): the refresh has a fixed
cost (opening the graph, the resolver's whole-graph pass) that a small rebuild
does not exceed by much. `analyze_codebase` declines the refresh when the
files to re-parse or purge exceed 40% of the tree (`touched * 100 > 40 *
files`): on `src/` it refreshes up to 182 files and rebuilds from 183, so the
40% row above, where the refresh measured 1.2 s faster, is rebuilt; no row
past it measured the refresh faster. Below the bound, on small trees, the
refresh can lose up to about a second to a rebuild. Not measured: a
tree larger than 456 files (ripgrep, django), another language, other hardware.

### Deletions: `--delete`, `src/` (460 Rust files)
Same sweep, but each share of the files is deleted instead of edited, so the
refresh parses nothing and only purges (review of #552, note 2). The share is
of the tree before the deletion, which is the bound's denominator: the current
files plus the deleted ones. 2026-11-19, same hardware and toolchain, release,
min of 3. **Command:** `cargo run -p incremental-speed-bench --release --bin
crossover -- --delete src`. **Results:** `results_crossover_delete.json`.

| deleted | files | incremental + resolve | full + resolve |
|---:|---:|---:|---:|
| 10% | 46 | 4054 ms | 18141 ms |
| 20% | 92 | 5628 ms | 15419 ms |
| 30% | 138 | 6165 ms | 13616 ms |
| 40% | 184 | 7130 ms | 11661 ms |
| 50% | 230 | 7511 ms | 9268 ms |
| 75% | 345 | 8844 ms | 4823 ms |

The refresh wins every row up to 50% and loses at 75%, so the 40% bound
declines no deletion the refresh measured slower on. Over the current files
alone, the 30% row (138 of 460, read as 138 of 322, 43%) was rebuilt in
13.6 s where the refresh took 6.2 s. Not measured: mixed edits and deletions,
renames, another tree size.

## Second call on an unchanged tree (issue #443): `second_call`

The issue asks that a second `analyze_codebase` on an unchanged repository be
at least 5 times faster than the first. A wall-clock ratio is not a gate here
(issue #74, above), so this binary measures it. It starts the server binary,
speaks MCP over its stdio as a client does, and calls `analyze_codebase` three
times with the same `path` and `output_dir`: the first call builds the graph
(`index.mode` = `full`), the next two find nothing changed and refresh it
(`incremental`, 0 files re-parsed). Each round starts from an empty
`output_dir`; the minimum of 3 rounds is kept per call. `total_ms` is the
server's `total_elapsed_ms`; `rest_ms` is that total less the search phase and,
on a refresh, the index phase, so on a refresh it is resolve plus cluster.

- **Hardware:** Linux x86_64, 4 CPUs (cloud container), 15 GiB.
- **Toolchain:** rustc 1.95.0 (`rust-toolchain.toml`), lbug built from source
  (`LBUG_BUILD_FROM_SOURCE=1 ZSTD_SYS_USE_PKG_CONFIG=1`, as in CI).
- **Date:** 2026-11-19. **Profile:** release, min of 3.
- **Command:** `cargo build --release --bin ai-architect-mcp-codebase`, then
  `cargo run -p incremental-speed-bench --release --bin second_call --
  target/release/ai-architect-mcp-codebase <corpus>`.
- **Results:** `results_second_call_ripgrep.json`, `results_second_call_src.json`.

| corpus | files | first (full) | second | third | first / second | index (2nd) | resolve + cluster (2nd) | search (2nd) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| ripgrep `fdf7d95` | 226 | 14944 ms | 3142 ms | 3078 ms | 4.76× | 1054 ms | 2028 ms | 60 ms |
| this repository's `src/` at `bf36835` | 461 | 22015 ms | 9657 ms | 9664 ms | 2.28× | 75 ms | 9419 ms | 163 ms |

Neither corpus reaches 5×. On a refresh that re-parses nothing, the index
phase costs 75 ms on `src/` and about 1 s on ripgrep, and the search index
under 0.2 s; resolve and cluster, which run over the whole graph on every call
(`analyze_handlers::do_analyze_codebase`), make 65% of the second call on
ripgrep and 98% on `src/`. The third call costs what the second does, so the
first refresh leaves nothing the next one redoes. Not measured: django (7,008
files, the issue's other corpus), another language, other hardware. Why the
index phase of an unchanged ripgrep costs 1 s against 75 ms on `src/` is not
measured either.
