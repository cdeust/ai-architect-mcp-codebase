# Wiki skeleton (#196): scoped mutation run and Completion Ledger

Slice 9 of issue #196, criteria 8 and 9. Run on 2026-11-14 on `7c71c86` (main after
slice 8, #538), with `cargo-mutants` 27.1.0, Rust 1.95.0, Linux x86_64,
`LBUG_BUILD_FROM_SOURCE=1 ZSTD_SYS_USE_PKG_CONFIG=1` (the `unit` command's build).

## Scope

The non-test source files that the eight slices of #196 added (`git log --name-only
--grep='#196' origin/main`, then `src/wiki/*.rs` and the handler), every mutant
`cargo mutants` generates for them:

| File | Caught | Unviable | Survived | Of which equivalent |
|---|---:|---:|---:|---:|
| `src/wiki/checks.rs` | 22 | 5 | 1 → 0 | — |
| `src/wiki/communities.rs` | 20 | 6 | 1 | 1 |
| `src/wiki/coverage.rs` | 3 | 1 | 0 | — |
| `src/wiki/error.rs` | 1 | 0 | 0 | — |
| `src/wiki/export.rs` | 0 | 1 | 0 | — |
| `src/wiki/god_nodes.rs` | 67 | 8 | 1 | 1 |
| `src/wiki/missed.rs` | 20 | 2 | 2 | 2 |
| `src/wiki/processes.rs` | 16 | 1 | 2 → 1 | 1 |
| `src/wiki/skeleton.rs` | 37 | 7 | 0 | — |
| `src/wiki_handlers.rs` | 10 | 1 | 1 → 0 | — |
| **Total (236 mutants)** | **196** | **32** | **8 → 5** | **5** |

`src/wiki/mod.rs` only declares modules and re-exports (no mutant).
`src/tool_schemas_wiki.rs` (the JSON schema of the tool) was left out: its only function
returns a literal schema value, asserted by the tool-catalog tests, not by the wiki tests.
`cargo mutants` lists every mutant twice, since `src/main.rs` and `src/lib.rs` both compile
the crate's modules; the counts above are distinct mutants.

Commands (the library files against the library's wiki tests, the handler, which only
the binary compiles, against its own tests):

```bash
cargo mutants --in-place --no-shuffle -f 'src/wiki/*.rs' -e '*_tests.rs' \
  --cargo-arg=--lib --cargo-arg=--locked --cargo-test-arg=wiki:: --minimum-test-timeout 60
cargo mutants --in-place --no-shuffle -f src/wiki_handlers.rs \
  --cargo-arg=--bin=ai-architect-mcp-codebase --cargo-arg=--locked \
  --cargo-test-arg=wiki_handlers --minimum-test-timeout 120
```

First run: 448 mutants listed (224 distinct) in 75 minutes for the library, 12 for the
handler. The three non-equivalent survivors were then killed by three new tests and the
survivors run again with the same commands restricted by `--re` to their functions.

## The three survivors killed

| Mutant | What the tests did not say | Test added |
|---|---|---|
| `checks.rs:50` `expecting` → `Ok(())` | the error for a `cross_community_edges` that is no object names the shape expected | `skeleton_tests::a_cross_community_value_that_is_no_object_names_the_shape_expected` |
| `processes.rs:63` `&&` → `\|\|` in `participation_tables` | membership is read from `ParticipatesIn_*` only: an `EntryPointOf` row alone adds no member | `processes_tests::an_entry_point_row_alone_makes_no_member` |
| `wiki_handlers.rs:141` `/` → `%` in `report_view` | each of the six report lists gets an equal share of one response section: the six fit in it, one more item would not fit the share | `wiki_handlers_tests::a_long_report_list_is_bounded_with_its_count` (two assertions added) |

## The five equivalent survivors

Each mutant below changes which tables a reader queries, never what it returns: the
extra rows are dropped by a filter after the query, or the extra query returns no row.
Equivalence is argued from the schema (`REL_TABLES`, `src/graph_store/schema.rs`) and the
code that follows the filter; the library's tests pass under each mutant, which is
consistent with it and does not prove it.

1. **`communities.rs:80:61`, `&&` → `||` in `membership_tables`.** The filter is
   `rel.starts_with("MemberOf_") && to == Community`. In `REL_TABLES` the eight
   `MemberOf_*` tables all end at `Community` and no other table ends at `Community`, so
   both predicates select the same eight tables. The two would differ only on a schema
   that adds a `MemberOf_` table to another label or another table into `Community`.
2. **`god_nodes.rs:233:35`, `||` → `&&` in `read_edges`.** The mutant also queries the
   tables with one end outside the member labels (`Defines_File_Function`, …). Every row
   is then kept only when both ends are clustered members
   (`members.contains_key(&source) && members.contains_key(&target)`, line 247); a node
   of a label that no `MemberOf_` table leaves is never a member, so no extra row passes.
3. **`processes.rs:183:35`, `||` → `&&` in `read_edges`.** The same as 2 for process
   pages: the extra tables' rows are kept only when both ends participate in the same
   process, and only `Function` and `Method` nodes can participate.
4. **`missed.rs:113:33`, `||` → `&&` in `site_definers`.** The mutant also queries the
   `Defines_*` tables that do not end at `CallSite`; the query pattern binds the target
   as `(cs:CallSite)`, so those tables return no row (the store answers with an empty
   result, not an error: the tests of `attribute_missed` run the query under the mutant
   and pass).
5. **`missed.rs:124:35`, `||` → `&&` in `site_definers`.** The mutant keeps the definers
   of sites that are not open sites of the store. Its only caller, `attribute_missed`,
   iterates the open sites and looks their definers up by key
   (`definers.get(site)`), so an entry for another key is never read.

## Completion Ledger

| # | Criterion | Status | Evidence on `main` |
|---|---|---|---|
| 1 | Zero network calls, asserted by a test | Done (slice 8, #538) | `src/wiki_handlers_egress_tests.rs`: `the_export_opens_no_socket_and_creates_no_process` runs the tool's export in a child process under a seccomp filter that kills `socket`, `connect`, `io_uring_setup`, `execve(at)`, `fork`/`vfork` and a `clone` without `CLONE_THREAD`; `the_filter_kills_a_connect_and_a_spawn` proves the filter kills both. Linux x86_64 and aarch64 only (`cfg` on the module). |
| 2 | No model client constructed, asserted by a test | Done (slices 1 and 8) | The same child process: a model client needs a socket or a spawned program, and both kill it. `skeleton_tests::the_wiki_module_names_no_network_or_model_client_api` scans `src/wiki` for network and model-client names. |
| 3 | Untagged edge unrepresentable | Done (slice 1, #506) | `Provenance` is a required enum field of `Edge`; `skeleton_tests::an_edge_without_or_with_an_unknown_provenance_does_not_deserialise`, `every_edge_is_serialised_with_one_of_the_two_provenance_tags`. |
| 4 | Every structural claim carries `file:line` | Done (slice 1) | `Location` refuses line 0 and an empty file; `skeleton_tests::every_structural_claim_carries_file_and_line`, `a_location_refuses_line_zero_and_an_empty_file`; claims without an anchor are returned in `ExportReport`, never dropped (`export_tests::what_the_skeleton_cannot_hold_is_reported_as_each_builder_returns_it`). |
| 5 | Coverage block populated, or present and empty | Done (slices 1, 4, 5) | `skeleton_tests::an_empty_coverage_block_is_present_not_absent`, `coverage_tests::the_artifact_shows_the_gap_and_an_explicit_empty_block`, `missed_tests::the_artifact_shows_the_missed_gap_and_an_explicit_empty_block`. |
| 6 | Byte-identical skeleton on the same graph | Done (slices 1–7) | `skeleton_tests::the_same_graph_in_any_order_gives_byte_identical_json`, `the_v1_output_matches_the_golden_fixture_byte_for_byte`; `communities_tests` / `processes_tests` `pages_of_an_indexed_and_clustered_repo_form_a_deterministic_skeleton`; `export_tests::an_indexed_repo_exports_every_page_with_its_gaps`. |
| 7 | Versioned schema, version in the artifact | Done (slice 1) | `skeleton_tests::the_schema_version_is_embedded_and_round_trips`, `another_schema_version_is_refused_on_read`. |
| 8 | Scoped mutation run: no non-equivalent survivor | Done (slice 9, this file) | 236 mutants: 196 caught, 32 unviable, 3 survivors killed by new tests, 5 documented above as equivalent. |
| 9 | Completion Ledger | Done (slice 9) | This table. |

Not covered by this ledger: the out-of-scope items of the issue (prose, cdeust/Cortex#358;
rendering, cdeust/cortex-viz#112; incremental regeneration, cdeust/Cortex#359), and the
release that would put the 28th tool in a published binary, which no routine makes
(`docs/agent-guidance.md`, "Issues", rule 4).
