# Wiki skeleton: scoped mutation run, part 1 (issue #196, criterion 8)

Criterion 8 of #196: a scoped mutation run on the changed files, with zero
surviving non-equivalent mutants, or each survivor documented as equivalent.
The run is split by file, since one mutant costs about 75 seconds here (an
incremental rebuild of the library's test binary); this record covers the
first five files. The remaining files are listed at the end.

## Command

Run on 2026-11-14 from a worktree of `origin/main` at `7c71c86`, with
cargo-mutants 27.1.0 and rustc 1.95.0, Linux x86_64:

```bash
export LBUG_BUILD_FROM_SOURCE=1 ZSTD_SYS_USE_PKG_CONFIG=1
cargo mutants --in-place --no-shuffle -o <out> \
  -f 'src/wiki/*.rs' -e '*_tests.rs' --timeout 120 \
  -- --lib --locked -- wiki
```

The test filter `wiki` selects the 83 tests of `src/wiki` (the library's
unit tests whose path contains `wiki`). The unmutated baseline passed
(98 s build, 5 s test). The run was stopped once it reached
`src/wiki/god_nodes.rs`; the two `god_nodes.rs` outcomes it recorded are not
counted here.

## Result

| File | Mutants | Caught | Unviable | Missed |
|------|--------:|-------:|---------:|-------:|
| `src/wiki/checks.rs` | 28 | 22 | 5 | 1 |
| `src/wiki/communities.rs` | 27 | 20 | 6 | 1 |
| `src/wiki/coverage.rs` | 4 | 3 | 1 | 0 |
| `src/wiki/error.rs` | 1 | 1 | 0 | 0 |
| `src/wiki/export.rs` | 1 | 0 | 1 | 0 |
| **Total** | **61** | **46** | **13** | **2** |

An unviable mutant does not compile (most replace a return value with
`Default::default()` for a type that has no `Default`); it cannot run, so it
cannot survive.

## Survivors

1. `src/wiki/checks.rs:50:13`: replace `Unique::expecting` with
   `Ok(Default::default())`. **Not equivalent**: the error that refuses a
   `cross_community_edges` written in another shape than an object loses its
   "expected a map of community ids to edge counts". **Killed** by the new
   test `a_cross_community_count_list_in_another_shape_names_the_map_it_must_be`
   (`src/wiki/skeleton_tests.rs`), which fails on the mutant (applied by hand
   and run: 1 failed) and passes on the code.
2. `src/wiki/communities.rs:80:61`: replace `&&` with `||` in
   `membership_tables`. **Equivalent**: the filter reads the constant
   `REL_TABLES` (`src/graph_store/schema.rs`, lines 145 to 358). Its eight
   tables whose name starts with `MemberOf_` all point at `NODE_COMMUNITY`,
   and no other table points at it (`NODE_COMMUNITY` appears ten times in the
   file: its definition, the node-table list, and those eight rows). Either
   condition alone selects the same eight rows, so no input tells the two
   operators apart. Both conditions are kept, and the doc comment of
   `membership_tables` says why.

## Not covered by this record

`src/wiki/god_nodes.rs`, `src/wiki/missed.rs`, `src/wiki/processes.rs`,
`src/wiki/skeleton.rs` (library), and `src/wiki_handlers.rs`,
`src/tool_schemas_wiki.rs` (binary: their tests are the binary's unit tests,
so they need `--bin ai-architect-mcp-codebase`). They are the next parts of
the same run.
