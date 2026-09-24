# Configuration

Options that change what gets indexed, where the graph lives, and how much
address space the graph database reserves. Tool arguments are documented in
each tool's JSON Schema (`src/tool_schemas*.rs`); this page covers the ones
that need more than a sentence.

## Contents

- [Team-shared graph artifact](#team-shared-graph-artifact)
- [Excluding directories from the walk](#excluding-directories-from-the-walk)
- [Database address-space reservation (`max_db_size`)](#database-address-space-reservation-max_db_size)
- [Measured graph sizes](#measured-graph-sizes)

## Team-shared graph artifact

`index_codebase` can commit a compressed snapshot of the graph, so teammates
who clone the repository do not have to index it from scratch. Both flags
below default to `false`, and without them `index_codebase` indexes from
source as usual.

With `"export_artifact": true`, after a successful index, `index_codebase`
writes a `tar` then `zstd` snapshot to
`<path>/.ai-architect-mcp-codebase/graph.zst`, plus a `graph.meta.json`
sidecar (schema version, git sha, tool version, node and edge counts). It also
appends a `.gitattributes` entry
(`.ai-architect-mcp-codebase/graph.zst binary merge=ours`) so the committed
binary never produces merge conflicts across branches. Commit both files.

A repository indexed before the project rename (issue #195) carries the
snapshot under the old `.automatised-pipeline/` directory name. The first touch
of the artifact (export, bootstrap, or a `hook-augment` Grep/Glob check)
renames it to the current name in place, once; there is no permanent read of
both paths.

With `"bootstrap": true`, when there is no local graph at
`<output_dir>/graph` but a committed artifact is present, `index_codebase`
decompresses the snapshot instead of indexing from source. It first compares
the artifact's git sha with the repository's current HEAD:

- Equal shas: the snapshot is imported as it is. The response carries
  `source='artifact_bootstrap'` and `graph_state='fresh'`.
- Different shas: by default the snapshot is imported and then filled
  incrementally up to the working tree, re-parsing only the artifact-to-HEAD
  diff. The response carries `source='artifact_bootstrap_fill'`,
  `graph_state='filled_to_working_tree'`, `fill_method` and the
  `{changed, added, deleted, renamed, unchanged}` counts.
- `"accept_stale": true` imports the stale snapshot and skips the fill. The
  response then carries a `stale_artifact`
  `{artifact_sha, head_sha, commits_behind}` report, so a stale graph cannot
  pass for a fresh one.

A fill that fails (no git diff and no bundled manifest) falls back to a full
index, and so does an import failure. Both are logged to stderr and reported
in a `bootstrap_skipped` note; neither leaves a silent partial graph.

## Excluding directories from the walk

Issue #249. `index_codebase` and `analyze_codebase` both accept
`"exclude_dirs"` (default `[]`): directory names or paths to prune from the
walk, on top of the built-in build and dependency skip list (`node_modules`,
`.venv`, `vendor`, `target`, and others). It is meant for directories that
must never be read, such as a secrets folder or a credentials mount.

An entry with no path separator (for example `"secrets"`) is a bare name and
matches anywhere in the tree, like the built-in list. An entry with a path
separator (for example `"config/secrets"`) is a path relative to `path` and
matches exactly one subtree. Globs are not supported.

Exclusion takes precedence over every `dependency_scope` tier, `full`
included: it is checked first, independently of dependency-directory descent.
Each pruned directory appears in the coverage sidecar as `skipped` with reason
`user_excluded`, and the response's `coverage.skipped.user_excluded_count`
carries the exact count. Changing `exclude_dirs` on an existing graph requires
`"full": true`, because the incremental-index manifest does not record it (the
same holds for `dependency_scope`).

Separately from `exclude_dirs`, a directory the OS refuses to read
(`EACCES` / `PermissionDenied`) does not abort the index. It is recorded in the
coverage sidecar with reason `unreadable` and the walk continues, so one
locked subdirectory cannot discard an otherwise successful index.

## Database address-space reservation (`max_db_size`)

Every LadybugDB `Database` this crate opens reserves virtual address space up
front, sized by `max_db_size`. lbug's own default (`SystemConfig::default()`)
is `1 << 43` bytes, 8 TiB per instance. With `graph_cache`'s
`MAX_CACHED_GRAPHS = 8` entries live in the read-path cache at once, the worst
case is 64 TiB of reservation (issue #25). `system_config()` in
`src/graph_store/config.rs` is the single point every
`GraphStore::open_or_create` call goes through, with this precedence:

1. `AP_LBUG_TEST_MAX_DB_SIZE` is for tests only. `.cargo/config.toml`'s
   `[env]` table sets it for every `cargo test` process (issue #21). It was
   512 MiB (`2^29`) and is now 4 GiB (`2^32`), after a full-AST test run hit
   the 512 MiB ceiling on 2026-08-09; the reasoning is in the comment above
   the setting. When present it always wins, so `cargo test` behaves the same
   whatever the production setting is.
2. `AP_LBUG_MAX_DB_SIZE` is the production override, unset by default. The
   value is in bytes, must be a power of two, and must be at least 8 MiB
   (lbug's own `BufferManager::verifySizeParams` floor). An invalid value is
   rejected with an actionable error at `GraphStore::open_or_create` time; it
   never falls back silently.
3. With neither variable set, the default is 8 TiB (`1 << 43` bytes). This is
   lbug's own `DEFAULT_VM_REGION_MAX_SIZE`, the engine's per-database VM-region
   ceiling on 64-bit desktop and server platforms
   (`lbug-0.19.1/lbug-src/src/include/common/constants.h`). It is an
   address-space reservation; disk and memory grow only with the data
   actually written. An earlier release capped the default at 8 GiB (issue
   #25, sized from the table below). That cap aborted any ingestion whose
   graph outgrew it and was removed on 2026-08-14 (0.11.1), so an index
   completes whatever the corpus size.

Set `AP_LBUG_MAX_DB_SIZE` to bound the reservation where address space is
constrained, for example in a container with a low `RLIMIT_AS`.

## Measured graph sizes

Measured on 2026-07-15 with `du -k` on every `graph` file found under
`~/.cache/cortex/code-graphs/*/graph`, `~/.cortex/ap_graph/graph` and
`**/.prd-gen/graphs/*/graph` on the maintainer's machine; top 10 of 75:

| Graph | Size |
|---|---|
| `repro-cortex-viz-deps` (cortex-viz with `node_modules`) | 473 MiB |
| `bench-c2-viz-deps` (cortex-viz with deps) | 472 MiB |
| `bench-c3-viz-pubapi` (cortex-viz, public API surface) | 460 MiB |
| `wt-windows-launcher-96-97-*` (Cortex worktree) | 147 MiB |
| `wt-homeostatic-*` (Cortex worktree) | 144 MiB |
| `wt-tools-drift-*` (Cortex worktree) | 143 MiB |
| `Cortex-wt-wiki-titles-*` | 142 MiB |
| `wt-findings-provenance-*` | 126 MiB |
| `anthropic-partnership-Cortex` | 126 MiB |
| `wt-ingest-provenance-*` | 124 MiB |

The 75 graphs totalled about 4.87 GiB. Every graph outside the top three
(which include `node_modules`) was under 150 MiB.
