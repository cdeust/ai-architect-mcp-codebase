# Changelog

All notable changes to this project will be documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- A receiver typed by a path written with more than one segment resolves to
  the owner that path names (#368). The static pass kept only the last segment
  (`let s = b::Set::new();` reached the resolver as `Set`), so a file that also
  defines `a::Set` got an edge to `a::Set::m` through the same-file preference.
  The hint now keeps the path as written (`CallSite.receiver_hint` holds
  `b::Set`) and the resolver keeps only a method whose owner the path names:
  `crate::R` from the root of the caller's crate, `self::R` like `R`,
  `super::R` from the parent of the caller's inline module, `<lib>::R` from the
  root of that library of the repository, and any other path as a suffix of
  the owner's module path within the caller's crate. The module path of an
  owner is the one its file's location gives (`src/a.rs` and `src/a/mod.rs`
  are `a`, `lib.rs`, `main.rs` and every target entry file are the root),
  followed by its inline modules. When several owners match, the call is
  ambiguous: no same-file preference applies to a written path. Paths that do
  not resolve (a `super` from the root of a file, a `#[path]` layout, a module
  re-exported by `pub use`, a module of another crate reached by a relative
  path) give no edge: those edges are lost, none is made wrong. A one-segment
  hint (`Set`, including one imported by `use b::Set;`) keeps today's
  behaviour: the case of a `use`-imported name next to a namesake in the same
  file is #380. Graphs written before this change carry last-segment hints, so
  the crate evidence form goes to 3 and an incremental refresh, a bootstrap
  fill or an artifact import asks for one full reindex.
- A receiver bound by `let x = Type::assoc(..)` is typed as `Type` only when
  `assoc` returns it (#370). The static pass read the type off the written
  path, so `let o = Opt::new(); o.m()` got an edge to `Opt::m` at 0.87 even when
  `new` returns `Option<Opt>`, `Result<Opt, E>`, `Box<Opt>`, `impl Trait` or
  another type. The parser now records the associated function beside the hint
  (`receiver_hint_via = "assoc:new"`) and the resolver keeps a candidate only
  when that function of the candidate's type declares `Self`, the type or the
  type with generic arguments as its return type, or when it is a variant of
  the candidate enum (`Response::Refused(..)` builds a `Response`). The type and
  its impl may sit in another file than the call: the check reads the declared
  return type the graph already stores. When the type's impl blocks are split
  over several files, a function found in another file than the type is not
  seen, and the call does not resolve (lost, not wrong). `#[cfg]` twins of the function must all agree. What no longer
  resolves: a `Box<Type>` constructor (its method calls go through `Deref`, so
  the edge was right; it is lost, not made wrong), a constructor inherited from
  a trait, a derive or a macro, and one with no declared return type in the
  graph. `Gen::<u8>::new()` and `Self::new()` still give no hint, as before. `?`,
  `.unwrap()` and `.expect(..)` stay outside the receiver hint. On dy-wcet
  v4.1.6 the 101 `TaskSet::response_of` sites resolve as before (99, none to a
  wrong target) and the 403 rows of the `receiver-local-binding` tier are
  unchanged. A graph written before this change holds hints no pass checked, so
  the crate evidence form goes to 2: an incremental refresh, a bootstrap fill
  and an artifact import of such a graph are refused until one full reindex
  (`index_codebase` with `full: true`), which rebuilds it.
- A graph whose totals cannot be read is no longer reported as an empty one
  (#361), and the note on the handle refusal says what each tool has already
  written when it arrives (#363). `index_status` read its counts through a
  helper that turned any failure into zeros, so while a running request held
  the graph's handle it answered `status: ok` with `node_count`, `edge_count`
  and `call_site_target_count` at 0. It now fails with the reason (the
  refusal code opens it); the bootstrap responses of `index_codebase` report
  `counts_unavailable` with the reason instead of zeros, and the incremental
  export skips the artifact rather than record zero totals in it. The note
  appended to the schema of the tools that can be refused said that nothing
  was written: that holds for the eight tools that open the graph once,
  before any write, but `analyze_codebase` can be refused when its resolve
  stage opens the graph (the graph is then indexed but unresolved) or at its
  final LSP check (after every stage wrote), and `lsp_resolve` when it
  reopens the graph to count its rows; each now says so. The list of those
  tools was kept by hand and did not include `index_status`; a test now holds
  a handle on a real graph, calls every graph tool through the dispatch table
  and requires the tools that return the refusal to be exactly the listed
  ones. That test found `ingest_traces` writing its `OBSERVED_CALLS` edges
  through the read cache's shared handle, the one a running request may hold,
  instead of the guarded open every other write tool uses, so it was never
  refused. It now opens the graph like the other write tools and is refused
  while another request holds the handle. The read cache's open is the only
  other open that does not release the cache, and every tool that uses it
  only reads. The probed tools are those whose input schema names or builds a
  graph, read from the registry, so a new graph tool without probe arguments
  fails the test instead of being skipped; the list and each tool's note are
  one table.
- `get_impact` points at the call sites that sit outside every compiled Cargo
  target even when the symbol has resolved callers (#318). The entry of
  `next_steps` that names them was only reached when `callers` was empty, so on
  dy-wcet `Response::meets`, which has library callers and four sites in the
  Kani harness `kani/response_bounds.rs`, it never appeared. It now appears
  whenever `unresolved_callsites_outside_targets` is above 0: it names the
  files, says the language server never loads them, and hands out a read-only
  `query_graph` statement that lists exactly the sites that count covers (the
  same filter), plus `query_graph(graph="missed")` for the file list. When every
  unresolved site is outside the targets, the hint no longer advises running
  the language server again.
- A receiver typed through `use crate::X` in a test, bench, example or binary
  target no longer reaches a library namesake (#357). In such a target, `crate`
  names the target, not the library: a test file whose root re-exports an
  external type (`pub use some_ext::Set;`) and reaches it through
  `use crate::Set;` got an edge to the library's unrelated `Set::answer` at
  0.85, because a `crate::`, `self::` or `super::` import counted as proof the
  type was local and the lookup went by last segment. The parser now records
  the whole path of such an import (`return-type-local-import:crate::Set`; a
  type defined in the module keeps `return-type`), and every index pass records
  each target's entry file, with its crate name for a library, and in
  `File.target_owners` the entries whose module tree reaches each file. For a
  file one of whose targets is not a library, a candidate is kept only when that
  target's own tree defines it, or, for exactly `crate::X`, when the target's
  root imports `X` from a library crate of the repository (then only that
  library's candidates); a re-export of any other path gives no edge, and a file
  reached by several targets must pass for each. A file owned only by library
  targets is resolved as before. **When the Cargo facts are unknown** (no
  `Cargo.toml`, `cargo` missing or failing) **or no target reaches the file,
  the lookup is not restricted**, as before: without cargo there is no
  non-library target to confuse, and declining would lose every `crate::` edge
  of a cargo-less tree. Not covered: a library root that re-exports an external
  type (#373), and a type re-exported more than one module away from the
  target's root, which declines. Graphs written before need the full reindex
  described for #358.
- A receiver typed through `use <crate>::X` is decided from the Cargo facts of
  the latest index pass (#358). The parser marks such a hint
  `return-type-import:<crate>`, and the indexer used to rewrite an accepted mark
  into a plain `return-type`, which no later run could check again: after a
  crate rename, an incremental pass kept the edge of every file it did not
  reparse, while a fresh full index of the same tree declined it, so the graph
  depended on the order of the runs. The mark now stays as written; every index
  pass, full or incremental, records the workspace's library crate names in a
  `GraphMarker` row (`crate_evidence`), and the resolver accepts the hint only
  when its crate is among the names recorded by the latest pass. Each
  `resolve_graph` first deletes every row of the `receiver-return-type` tier and
  opens its call sites again (rows of every other tier are untouched), so each
  site is decided again from the current facts. The language-server pass still
  resolves a site the static pass declined, from rust-analyzer's own
  definition, as it does for any open site. No Cargo facts (no `Cargo.toml`,
  `cargo` missing) still means no crate name, so such a hint is declined, as
  before. **A graph written by an earlier build needs one full reindex**: an
  incremental refresh, a bootstrap fill, an artifact import and a library full
  index over it are refused with "full reindex required" (a `crate_evidence_form`
  marker row, written last by a full index, as for #354), since an earlier pass
  may have rewritten a hint as accepted. Reads are unaffected.
- `scripts/check_moved_fn_bodies.py`, the proof that a code move changed no
  function body, could pass a change it should catch (#362). It now fails with
  exit 2 when either side has no function, so a mistyped ref or path no longer
  compares nothing with nothing and passes. The compared text of a function
  starts at its outer attributes and doc comments and includes its visibility
  and qualifiers, so dropping `#[test]` or changing `#[should_panic(..)]`,
  `#[cfg(..)]`, `///` or `pub(crate)` is reported. Whitespace is collapsed
  outside literals only: string, raw string (`r#".."#`, any number of `#`),
  byte string and char literals are compared byte for byte, and the scanner
  reads raw strings, nested block comments and char literals such as `'\''`
  while leaving lifetimes such as `'a` alone. CI now also runs the script end
  to end on a fixture pair kept in `scripts/tests/fixtures/moved_fn_bodies`: a
  pure move must pass and a copy without its `#[should_panic]` and with one
  space removed inside a string literal must fail; the script before this
  change passed that copy; the step requires exit 1 exactly, so a broken
  fixture path (exit 2) cannot pass for a caught change. A `;` inside an array
  type of a signature (`[u8; 4]`) no longer ends the declaration early, which
  let a changed body pass. On the real split of `src/graph_cache.rs` in #352
  (commit `560bc83`) the hardened script still exits 0, 16 functions on each
  side.
- The benchmark no longer carries a label for a deleted file, and a label that
  names a deleted path now fails `cargo test` (#359). Label q9 of the
  `rust-self` corpus queried `security_gates.rs`, which #262 split into
  `security_gates/mod.rs` and `security_gates/gates.rs` on 2026-08-25; the
  harness refused every full run with exit 3 since then. The label is replaced
  by one label per new file, each re-derived from the file's `use` statements
  and checked against the imports a fresh index reports (they agree). The
  staleness guard of #132 only ran inside a full benchmark run; a unit test of
  the benchmark crate now loads every corpus under `benches/corpora` and
  asserts that every source path and fixture path a label names exists. The
  guard also read only `.rs` paths out of queries, so the `f.path = 'app.ts'`
  labels of the TypeScript corpus were never checked; it now reads the
  extensions of the languages the corpora cover (Rust, TypeScript,
  JavaScript, Python, Go, Kotlin), and it flags an absolute path, which
  passes only on the machine that wrote it. Measured with `bench_end_result
  --all` on the same server binary, before and after: exit 3 (stale label)
  becomes exit 1 (score below target), the aggregate goes from 0.747 to 0.753,
  `rust-self` from 0.613 to 0.624 (its q9 from 0.672 to 0.793) and
  `typescript-small` stays at 0.881. The gap to the 0.85 target remains and is
  the subject of #214.
- A call that names a type gets a per-site row (#356). A call to a tuple-struct
  constructor such as `Tier(1)`, or a class instantiation, was marked
  `is_resolved` and had its symbol-level `Uses_Function_Struct` or
  `Uses_Method_Struct` edge, but no row in any `Calls_CallSite_*` table, because
  none targeted a Struct: a consumer that counts per-site rows to measure
  resolution undercounted those sites, and a purge could not reopen them. The
  new table `Calls_CallSite_Struct` holds the row, written by the static resolver
  and by the language-server pass beside the symbol-level edge, which stays as it
  is. `edge_count` does not move (a per-site table counts in
  `call_site_target_count`, by the shape rule of #338), and the table list of
  the purge is derived from `REL_TABLES`, so a constructor site whose struct file
  is deleted is reopened; the accepted gap noted in #353 stays open only for macro
  sites and for language-server sites that point at a Trait or an Enum.
  `get_impact` on a Struct lists the functions and methods that construct it
  under `users`. A graph written by an earlier build gets the table when it is
  opened for an incremental refresh, a bootstrap fill, `resolve_graph` or
  `lsp_resolve`, empty, and the next resolve pass backfills every row: no
  reparse, no marker, no full reindex. A qualified call whose qualifier is
  `Self`, or names only enums and type aliases (`Kind::A(1)`, `type K = Kind;
  K::A(1)`), no longer resolves to a struct `A` found by name elsewhere: it
  builds a variant, which the index does not hold, and the by-name lookup used to
  take it for a lone struct (an edge that already existed in the graph, now
  refused). A qualifier that also names a module, a struct or a trait keeps the
  lookup open. `Self` is not resolved to the enclosing impl type. Not covered: a
  call that resolves to an Enum, a Trait or a TypeAlias still has no per-site
  row; a variant brought in with `use Kind::*` and called bare, and a type alias
  or enum renamed by `use x as K`, are not told from a struct; an incremental
  refresh or a bootstrap fill creates the table but does not resolve, so its
  rows appear at the next `resolve_graph`.

- A receiver that spells its own type is typed statically (#355). The static
  resolver typed a receiver bound by `let t = Type::new(..)` but not a tuple
  constructor `Tier(1)`, a struct literal `Named { n: 3 }`, or a receiver written
  in place (`Tier(1).join(..)`, `Tier::new(1).join(..)`, `Named { n: 3 }.get()`),
  although the type is in the expression; `lsp_resolve` resolved them all. The
  parser now reads the type off the expression, as a third source tried after
  the binding and the return type of a free function. A tuple constructor or a
  struct literal, bound by one untyped `let` or written in place, resolves at
  `receiver-local-binding` (0.87). `Type::assoc(..)` written in place resolves at
  `receiver-return-type` (0.85), only when the file holds exactly one inherent
  impl of the type with exactly one function of that name, not `async`, declared
  to return `Self` or the type. For an in-place receiver the resolver reads the
  method name after the last dot of the callee text, and only when the hint
  comes from this source. Everything here declines on doubt: the type must be
  defined once in the file, in a scope the call sees (the caller's module, an
  enclosing block, or the parent module through `use super::*;`), with no type
  parameters, and with no function, `const`, `static`, alias, `use`, local
  binding or macro that may bind the same name. A struct reached only through a
  `use`, a path before the name, `Self`, an enum variant, a turbofish, a trait
  impl, an impl in another file, or an associated function returning
  `Option<Self>`, `Result<Self, _>`, `Box<Self>` or another type gives no hint.
  For this source the resolver keeps a candidate only when it is a `Method` whose
  owner, the qualified name before the last `::`, is a `Struct` or `Enum` defined
  in the caller's own file, because the hint is the last path segment (#368): a
  namesake type in another file, a `trait Tier { fn join }` or a `mod Tier` of
  the same file, and an `impl other::Tier` are never candidates. The price is
  recall: an impl in a module other than the one that defines the type, an impl
  in another file, and a `union` are not resolved by this source (the language
  server still resolves them). The method of a trait implemented for the struct
  in the same file is a method of that struct and resolves. Two structs of the
  name in one file decline in the parser. A site of these shapes had no hint before, and its callee text (`t.join`, `Tier(1).join`, `Tier::new(1).join`) names no entry by
  name, so the by-name path had no target to give it. A hint that finds no
  candidate ends the resolution of the site, as for every other hint. Inside a macro argument (`assert_eq!(Tier(1).join(..), 3)`) the scan
  rebuilds `(1).join` from the token tree and finds no receiver, so those sites
  stay unresolved. `let t = Tier::new(..)` is unchanged (its type is still taken
  from the path alone, #370). Files that did not change keep the empty hint they
  were indexed with until a full reindex, because an incremental refresh is keyed
  on the content hash: edges are missing on them, none is wrong.
- `get_impact` tells production callers from test, bench and example callers
  (#354). Until now the list held every caller of a function in one list and
  `callers_total` counted them all, so on a well-tested function most of the
  number was test code. Each caller now carries a `context`: `production`,
  `test`, `bench`, `example`, `proof`, or `unknown`. Two facts decide, and a
  caller is non-production when either says so. The source: a function is `test`
  code when it has a test attribute (`#[test]`, `#[rstest]`, `#[test_case]`,
  `#[wasm_bindgen_test]`, or the `test` macro of `tokio`, `async_std`,
  `actix_rt`, `actix_web`, `sqlx`, `test_log`, `futures_test` or `smol_potat`,
  with or without arguments; a `#[other::test]` from a crate not in that list
  stays unmarked, because calling production code a test would hide its callers)
  or when a `#[cfg]` that reaches it requires
  `cfg(test)` (an item, `mod`, `impl` or `fn` under `#[cfg(test)]`, an inner
  `#![cfg(test)]`, `cfg(all(test, ..))`); `#[bench]` marks `bench`,
  `#[kani::proof]` marks `proof`; a function nested in one of those belongs to
  it. `cfg(any(test, ..))`, `cfg(not(test))` and `cfg_attr(test, ..)` decide
  nothing. The Cargo package: from `cargo metadata` and the module tree, a file
  is `test`, `bench` or `example` when it is reached only from `tests/`,
  `benches/` or `examples/` targets and the modules they declare, or only through
  a `#[cfg(test)] mod x;`; it is `production` when a lib, bin or build target
  reaches it, and any production path wins. A file called `tests.rs` that
  `lib.rs` declares as a plain `mod tests;` is production: no name is guessed.
  Nothing is decided by reachability, so a helper that production and test code
  both call keeps the context of where it lives. The answer adds
  `callers_by_context` (a count per context), `callers_production_total` and
  `dependents_production_total`, and `code_context_basis` (`cargo+source`,
  `source_only` when there is no Cargo map, `absent` on a graph without the
  columns). `callers`, `callers_total`, `dependents_total` and `counts` keep their
  meaning and still count every caller; the tabular projection of every section
  gains a last column, `context`, null outside `callers`. `unknown` (nothing
  decided: a Python or TypeScript caller, a Rust file no target reaches, a
  `kani/` directory outside every target) counts as production in
  `callers_production_total`, so the count only shrinks when a caller is proven
  non-production, never on missing evidence. The reproduction of the issue
  (`fx2b`) now gives `callers_total: 7`, `callers_production_total: 1`.
  `entry_kind` now also marks `#[tokio::test]` and the other test attributes
  (with or without arguments), so those functions are entry points of the test
  processes like a plain `#[test]`; helpers keep an empty `entry_kind`. The
  graph gains `Function.code_context`, `Method.code_context` and
  `File.target_context`, and a `code_context_form` marker row written last by a
  full index. **A graph written by an earlier build needs a full reindex before
  an incremental refresh or a bootstrap fill** (the tool says so and refuses),
  because an unchanged file would keep an empty `code_context`; `get_impact` on
  such a graph still answers, with `code_context_basis: "absent"` and every
  caller `unknown`. `File.target_context` is rewritten on every index pass, so
  a `Cargo.toml` edit that adds a test target changes it without touching a
  file. `#[cfg(test)]` modules declared inline inside another inline module are
  not followed for the file class (the source still marks their functions).
  Limits: a file that lib code pulls in through `include!` or `cfg_if!` and
  that a `tests/` file also loads with `#[path]` is classed as test, because
  the lib path is not followed, so its production callers would be counted as
  non-production; the module walk follows `mod x;` declarations only. Each
  caller now carries a `context` string, so a byte-capped page of `callers`
  holds fewer callers than before; the totals and `next_offset` are computed
  before the cap and stay correct. An incremental pass over no Rust file
  clears `File.target_context`, and every caller then reads as `unknown`,
  which counts as production.
- A receiver name bound more than once in a function is typed through the
  binding that is live at the call (#350). `let s = Set::new(); ..; let s =
  Set::new(); s.answer(3)` was left unresolved by the static pass even when
  every binding had the same type, because a name bound twice was declined
  wherever it was used. The live binding is the one whose scope holds the call
  and whose binding point ends before the call starts, the latest such point
  winning: a `let` reaches the rest of its block, a function or closure
  parameter its body, a `for` pattern its loop body (not the iterated value),
  an `if let` or `while let` pattern the rest of its condition and its
  consequence or body (never the `else`), a match arm pattern its arm. On the
  local clone of `dy-wcet` v4.1.2 (the crate of the issue, whose file is
  `tests/adversarial.rs`) the three `response_of` sites of the function that
  binds `s` three times go from unresolved to resolved, each to
  `TaskSet::response_of`, and no `response_of` site of the crate has another
  target. The tiers and their confidences are unchanged (0.87 for a receiver
  typed by a constructor or a written type, 0.85 through a return type),
  chosen by how the live binding gets its type. Nothing is guessed: no hint
  when no binding reaches the call, when the live binding has no type (a
  closure parameter without a type, a destructuring pattern, a `for`, `match`
  or `if let` pattern, a `let` that is not a constructor call), when a
  binding has a form outside that list, or when a macro or an item (`const`,
  `static`, `use`) may bind the name; a name used in a match guard is still
  declined. The path of a name bound once is unchanged. The change is in the
  parser, so an incremental refresh, which is keyed on the content hash of a
  file, keeps the old empty hints of files that did not change until a full
  reindex: edges are missing there, none is wrong.

- A call to twins of one item under mutually exclusive `#[cfg]` gates is
  resolved to the twin the build compiles when the build decides it (#353,
  second of two changes). On the reproduction of the issue (`fast` off by
  default) the call from `caller` now has one edge, to
  `src/lib.rs::pick#cfg(not(feature=fast))`, with
  `resolution_method: "cfg-selected"` at confidence 0.85; the twin under
  `feature = "fast"` has none. Two facts decide, both without guessing. First,
  the caller's own gate: a caller whose id carries a gate (a twin, or an item in
  a twin `mod`, `impl` or `trait`) reaches the twin whose gate it contains, and
  never one it contradicts; a caller under `#[cfg(kani)]` that has no twin has
  no gate in its id and decides nothing. Second, the default features:
  each index pass (full, incremental, bootstrap fill) writes `cfg_active`
  (`active`, `inactive` or `unknown`) on every twin from the features `cargo
  metadata` reports as enabled by default for the package that compiles its
  file. Only `feature = "..."` leaves are decided, so a twin behind `kani`,
  `test`, `unix` or a `target_*` option stays `unknown`; a file reached only
  through a `mod` the default features compile out holds only `inactive` twins;
  no readable `Cargo.toml`, a file two packages compile with different
  features, or an unreadable gate give `unknown`. A twin is chosen only when
  exactly one twin is not ruled out and that one is shown compiled; otherwise
  the site stays open with the reason `cfg_twins`, as in the first change. The
  0.85 is a policy value, not a measurement: it sits below
  `import-scope-lookup` and never above 0.9, because the choice also assumes
  that the build under analysis is the default one. Every resolve first
  deletes the `cfg-selected` rows of the earlier run and reopens their sites,
  so an edit of `Cargo.toml` moves the edge without any file being reparsed.
  `get_impact` and `get_symbol` accept the bare name of a twinned item and
  resolve it to the twin that is `active`, or list the twins; an answer for a
  twin carries `cfg_gate`, `cfg_active` and `cfg_twins`, `get_impact` adds
  `unresolved_callsites_cfg_twins` and is never `exact` for a twin, and
  `index_status` reports `cfg_twins` and lists the twins the default build
  compiles out under `coverage.feature_gated.items`. `cfg_active` belongs to
  the default profile only: `BuildProfile` is where a Kani profile would plug
  in, and none is written. Not covered: twin files chosen by `#[cfg_attr(..,
  path = "..")]`, twins in different files (an ordinary ambiguity), and the
  language-server tier. The language server may still point a site at a twin
  the default build does not compile, and a site that a resolve chose a twin
  for and that the language server also pointed at another twin ends with both
  edges after the next resolve (the reset reopens the site, the language-server
  edge is not removed). Dependency feature unification is not modelled either:
  a caller in crate B into a twin of crate A is judged by A's default features
  while the build of B may enable other features of A, so the chosen twin can be
  the one B's build drops; 0.85 is a policy value, not a measurement. A graph
  written by the first change has no `cfg_active` column: an incremental
  refresh or a bootstrap fill adds it before the nodes are written, its twins
  read as `unknown` until then, and the ids do not change.

- Two Rust items of one name under mutually exclusive `#[cfg]` predicates are
  two nodes, and a call to that name is no longer resolved to either (#353,
  first of two changes). A file with `#[cfg(feature = "fast")] fn pick` and
  `#[cfg(not(feature = "fast"))] fn pick` used to give one `pick` node (the
  first) and an edge at 0.95 from every caller to it, whichever twin the build
  compiles. Now each twin is a node whose id ends in `#cfg(<gate>)` (for
  example `src/lib.rs::pick#cfg(not(feature=fast))`) and carries a new
  `cfg_gate` column. The gate is the item's own `#[cfg]` together with those of
  its enclosing `mod`, `impl`, `trait` and `fn` and the inner `#![cfg]` of its
  file or module, in a canonical order, without comments; `cfg_attr` is not
  expanded. Only items that collide in one file are renamed. The parse output of
  a file without twins is unchanged. A check that is run by hand shows it:
  `python3 scripts/check_parse_identity.py` fingerprints the 406 Rust files of
  `src/`, `tests/` and `crates/` with the parser of main and with this one and
  finds no difference; `--mutate` alters one file and makes it fail. An earlier
  ad hoc run also walked `benches/`, `benchmarks/`, `fuzz/` and `scripts/` and
  counted 424; the script fixes the corpus so the number is reproducible. It is
  not part of CI, because it needs a worktree of the base ref and two builds. A
  unit test shows that the twin logic does nothing on those files. What does change for every graph is that each node of
  ten tables gains the `cfg_gate` column, empty unless the node is a twin, and a
  `GraphMarker` table records the version of the canonical form of the gates.
  A call whose candidates are all twins of one
  item gets no edge and the reason `cfg_twins` on its call site, and so does
  every other edge that a name lookup would have pointed at the first twin
  (`Implements`, `Uses`); an `impl` for a twin type owns its methods only when
  its own gate is exactly the gate of one twin. A graph indexed before this
  change has already lost its twins, and one with the columns but no current
  marker names them by an older form, so an incremental refresh of it, an
  artifact import and a re-index over its directory ask for a full reindex
  (`index_codebase` with `full: true`). A call site whose resolution edge the incremental purge takes away (its
  target file deleted, renamed or rewritten without the target) is now reopened
  by the purge itself, in the same refresh; a site with no edge but a legitimate
  resolution (a tuple-struct constructor call, a macro or language-server site)
  keeps its flag. Choosing the twin the build compiles
  is left to the second change.

- A graph queried before `lsp_resolve` no longer loses the rows the pass writes
  (#352). The read cache keeps a graph handle open between requests, and a
  write tool opened its own handle to the same graph in the same process.
  LadybugDB gives each handle its own view: closing one runs a checkpoint of
  that handle's state (`Database::~Database`, lbug 0.20.4), so the handle that
  closes later writes its view over what the other one committed. So
  `lsp_resolve` answered `completed`, and the cache's next refresh or the end of
  the session removed its edges and per-site rows while every `is_resolved` flag
  stayed true. Every open, rewrite or removal of a graph, and the artifact
  import, now releases the cached handle for that graph first, whatever the
  spelling of its path; if a running request still holds that handle the open is
  refused with `graph_handle_in_use` naming the graph, and a release that finds
  the cache busy fails with `graph_cache_busy`. `lsp_resolve` and the LSP phase
  of `analyze_codebase` then close their handle, reopen the graph and count the
  `lsp-definition` rows again: `lsp_resolve` returns `persisted.lsp_rows` and
  fails with `lsp_rows_not_durable` on a difference, and `analyze_codebase`
  reports `lsp_status.state: failed` with that error. The count itself fails on
  a schema drift instead of reading zero. That check covers a loss that happens
  before the call returns; the release is what stops the later one. The server
  must stay single-threaded for the release to reach the cache, and a test now
  fails if production code starts a thread or joins a pool outside the LSP
  frame reader, wherever it sits relative to a `#[cfg(test)]` block. The two
  refusal codes are documented in the description of the eight tools that open,
  rewrite, remove or import a graph, and the bridge logs a sibling graph it
  skips instead of skipping it silently. `scripts/check_moved_fn_bodies.py`
  checks that a code move changed no function body.

## [0.13.0] — Per-site call rows, one edge count, one target per macro call

This is a minor release: the graph gains a column (`CallSite.macro_arg_shape`), the
responses gain fields (`call_site_target_count`, `analyze_codebase.graph`,
`macro_sites_count`, `row_limit`), and `edge_count` is now defined without the
per-site rows, so it reads lower than 0.12.0 on the same code.

### Fixed

- The per-site call tables are now written (#335). `Calls_CallSite_Function`,
  `Calls_CallSite_Method` and `Calls_CallSite_StdlibSymbol` were declared in the
  schema but no writer existed, so a resolved `CallSite` had `is_resolved = true`
  and no row naming its target. The static resolver, the macro-expansion pass and
  the language-server pass now each write the per-site row beside the
  definition-level `Calls_*` edge, with the same confidence and method. These
  rows restate a resolution and are not counted in `resolve.total_edges`.
  `get_context` leaves them out of `calls` and `called_by`, and a macro call site
  that resolved now has `is_resolved` set. A bare call in Python is now resolved
  by Python's scoping and never to a method (a module function that shares a
  name with a method was dropped as ambiguous, and a call whose only same-named
  symbol was a method got a false edge). The accuracy scorer counts a resolution
  once, against the symbol-level edge.
- `query_graph` now reports `truncated: true` when the row bound cut a result
  (#334). A query with no `LIMIT` ran with `LIMIT 500` and returned 500 rows with
  `total_count: 500` and `truncated: false`; only `limit_injected` hinted at the
  cut, and no offset could reach row 501. The injected bound now reads one probe
  row past the window, so a continuing result is reported and `next_offset` is
  the end of the window. The response carries `row_limit: 500` whenever the bound
  was injected, and `total_count` is a lower bound in that case.
- A macro call site now gets the one target its expansion calls, or none (#339, #342).
  The macro layer wrote every target of an expansion as a call, whatever the
  receiver: `write!` on a `fmt::Formatter` also got `io::Write::write_fmt`,
  `writeln!` on a `BufWriter` also got `fmt::Write::write_fmt`, and `vec![0; n]`
  got `Vec::new`, `Vec::push` and `Vec::with_capacity`, all at 0.85 on a site
  marked resolved. `write!` and `writeln!` are now decided by the declared type
  of their first argument, placed in the scope of the file that holds the site
  (confidence 0.8, method `macro-expansion-receiver-type`). A type defined in the
  file, or imported from a path that is not std or core (tokio `File`, futures
  `Sink`), is never the std one; a name imported from std or core, or written as
  a std path (`std::fs::File`, `fmt::Formatter` with `use std::fmt`), is; an
  alias resolves through its original path; a prelude name (`String`, `Vec`) is
  std unless the file redefines it. A `Formatter` defined in one file does not
  affect a std `Formatter` in another. Everything else is undetermined: a field,
  `impl Trait`, `dyn Trait`, a generic parameter, a local with no declared type
  or whose initialiser is not a constructor call of its own type,
  a name that nothing in the file places, a name that two `use` items bind to
  different origins, and a destination whose function body holds a `use`
  mentioning its type. The write traits a file imports do not decide anything:
  what is in scope says what the file may call, not what the destination is, and
  no class of sites makes that guess right by construction. A local built by
  `T::new(..)`, `T::create(..)`, `T::open(..)`, `T::with_capacity(..)` or
  `T::connect(..)` takes the type `T`, also through `?`, `.unwrap()` and
  `.expect(..)`, which return the value the constructor makes; any other
  wrapper, `x.map(..)` for one, does not. `vec![]` is
  `Vec::new` and `vec![x; n]` is `vec::from_elem`; a `vec![a, b]` list has no
  stable target. A site with no determined target gets no edge and no per-site
  row, stays unresolved and is reported as `destination type not determined` or
  `callee depends on the arguments of the macro` or
  `expansion calls compiler internals whose paths change between versions`.
  Every resolve first deletes the
  macro-expansion rows of earlier runs, so a graph written by an older build is
  corrected on its next resolve. Macro sites are no longer sent to the language
  server, which answered with the macro's own definition; `lsp_resolve` and
  `lsp_status` report them as `macro_sites_count`. The CallSite table gains a
  `macro_arg_shape` column. Only Rust sites are macros: Ruby keeps the `!` of
  `user.save!` in its callee name, and the plain call phase, the reset and the
  language server query no longer take such a call for a Rust macro. A graph
  indexed before this change stores the last segment of the type only: its
  qualified destinations such as `fmt::Formatter` lose their edge until the next
  index, and a bare name can still be placed wrongly, for example a `File` read
  as std because the file imports `std::fs::File`, when the source wrote
  `tokio::fs::File` in full. Re-index to get the qualified type.
- The Rust macro table names only a callee that every form of the macro reaches
  (#344, #346). `println!` and `eprintln!` also listed `Arguments::new_v1`, an
  internal the compiler no longer emits, and `assert!`, `debug_assert!`,
  `panic!`, `todo!`, `unimplemented!` and `unreachable!` listed one
  `core::panicking` function although the callee is `panic` or `panic_fmt` by
  the arguments and the edition. Each callee was compared with the expansion of
  rustc 1.93 to 1.98 and the std source of 1.95. `new_v1` is gone. The six
  macros whose callee depends on the arguments have no target: a site of one is
  an unresolved reference with the reason "callee depends on the arguments of
  the macro", not a resolved edge to a guess. The list form of `vec!` keeps no
  target, now with its own reason, "expansion calls compiler internals whose
  paths change between versions". The four comparison asserts keep
  `assert_failed`, which every form calls, and `debug_assert_ne!` gains its
  entry. `resolution_rate` can move either way on the same code. It goes down
  where a site that used to be resolved to a guess is now reported unresolved:
  `panic!`, `todo!`, `unimplemented!`, `unreachable!`, `assert!`,
  `debug_assert!`, a `write!` or `writeln!` whose destination type is not
  determined, and a list `vec!`. It goes up where a macro that calls nothing
  leaves the count (#345 below). The next resolve also deletes the
  `StdlibSymbol` nodes that the purge of old macro rows leaves with no
  relationship, so a graph written by an older build loses `new_v1` and the
  old `panic` node; a node any edge still uses stays.
- A macro that calls nothing is no longer an unresolved reference (#345, #346).
  `matches!`, `include_str!`, `include_bytes!`, `concat!`, `stringify!`, `env!`,
  `option_env!`, `cfg!`, `line!`, `file!`, `column!` and `module_path!` expand to
  a `match` or a literal. Their sites had no table entry and counted against
  `resolution_rate`. They are now in neither `total_refs` nor `unresolved`, get
  no edge, keep `is_resolved = false`, and are reported as `no_call_macro_sites`
  in the `resolve_graph` result and the `analyze_codebase` `resolve` block. They
  no longer count in `lsp_status.macro_sites_count`. A call written inside the
  arguments (`matches!(f(x), ..)`) is still a call site of the enclosing function.
- `edge_count` no longer counts the per-call-site rows (#338, #341). Since #335 filled
  the `Calls_CallSite_*` tables, `index_status.edge_count` summed them and grew
  by about 3 percent with no new call in the code, while `analyze_codebase`
  reported an `index.edge_count` taken before resolve ran, so one graph gave two
  different totals. An edge is now a relationship row that states a fact of its
  own; the per-site rows restate a resolution the symbol-level `Calls_*` edge
  already records and are reported as their own figure, `call_site_target_count`,
  beside `node_count` and `edge_count` in `index_status`, `analyze_codebase` and
  the history export. `analyze_codebase` gains a `graph` block read after the
  last phase, so it agrees with `index_status`; its `index` block stays the
  snapshot taken at the end of indexing. On a graph written after #335,
  `edge_count` is lower than the value 0.12.0 reported by the number of
  per-site rows. `resolve.total_edges` keeps its name and counts resolved
  references. An artifact sidecar exported before this change still records the
  old `edge_count`, per-site rows included. A relationship table that cannot be
  queried counts as empty, so on a damaged graph the totals are a floor.
- A bare function call inside macro arguments is now extracted (#328).
  `assert_eq!(helper(1), 2)` produced no `CallSite`, so `get_impact` on `helper`
  missed the test and reported no gap. The macro token-tree reconstruction now
  also matches an identifier directly followed by a `(` group. Look-alikes are
  rejected, each checked against the pinned tree-sitter-rust grammar: a nested
  macro (`vec![..]`), an index or block group, a name after `.` or `::`, an item
  being defined, an attribute body, and keywords that parse as names inside
  macros. A name bound locally and called like a closure inside a macro is
  deliberately not emitted, because the resolver binds a closure call to an
  unrelated top-level function of the same name. Calls headed by `self`,
  `super` or `crate`, and bare turbofish calls, are still not extracted inside
  macros.
- A `fn` declared inside a function body is now indexed (#327). It got no node,
  and its calls were credited to the enclosing function. A nested fn is a
  `Function` named `{enclosing qualified name}::{name}` (for example
  `src/lib.rs::S::m::gcd`), and calls in its body belong to it. This also removes
  a false caller edge: when the file imported a function of the same name, calls
  to the nested one resolved to the import. The resolver now lets an unqualified
  call reach a nested fn only from the function that declares it or one that
  encloses it, as Rust shadowing does, and removes nested fns from every other
  candidate set. A `#[test]` on a nested fn is not an entry point (rustc never
  runs it). Fn-local `impl`, `struct`, `mod` and `const` items are still not
  indexed.
- A receiver bound once and used inside a closure now keeps its
  `receiver_hint` (#329), so `(0..3).map(|i| x.m(i))` resolves statically to
  `T::m` when `x` is bound once by `let x = T::new()`. The scope search stopped
  at the nearest closure, and untyped closure parameters (`|x|`) were not counted
  as bindings, so a closure parameter that shadows an outer name would have
  received a false hint had only the first cause been fixed. A name bound once
  outside a closure and again inside it counts as bound twice and gets no hint.
  Closure parameters such as `i` are no longer emitted as call sites by the
  argument scan, which removes false caller edges to unrelated functions of the
  same name.

### Changed

- The README is rewritten around what this server does, with the operational
  detail moved under `docs/` (#340). The changelog notes for 0.12.0 were
  corrected (#333).

## [0.12.0] — Honest coverage for Rust builds; static receiver-call resolution; read-tool freshness receipt

Minor, not patch: this release adds backward-compatible functionality — new
response fields, new coverage buckets, a new `lsp_status.state` value, static
receiver-call resolution, doc-content search — alongside a large set of
correctness and security fixes. Most of the fixes come from a partner
verification of the server against a real Rust corpus (DYResearch/dy-wcet
@ `1e93ccd`), which found the server answering "exact, 0 callers" and
"`status: ok`" about code it had never resolved or never seen. The common thread:
**an absence of evidence is no longer reported as evidence of absence.**

Two changes an integrator should act on:

- **Re-index graphs built by 0.11.x.** A saved graph without the
  `Function.entry_kind` column (#273) is refused by the incremental, artifact
  import and fill paths with an actionable error; `index_codebase` rebuilds it
  from source. A graph whose freshness sidecars predate `meta.json` schema 3
  reports `graph_freshness.state: "unknown"` until re-indexed (see Security).
- **`get_impact`'s `epistemic: "exact"` is now earned, not defaulted** (#299,
  #316). It requires a coverage record with zero gaps in every bucket and a
  known Cargo target map. Expect `lower-bound` more often, each time with a
  reason naming the cause.

New MCP-contract fields at a glance (all additive; no existing field changed
shape):

| Surface | Field | Issue |
|---|---|---|
| `search_codebase`, `get_symbol`, `get_impact` | `graph_freshness` `{state, dirty_files, checked_files, commits_behind, commits_ahead}` | fleet-watch#112 |
| `index_codebase`, `analyze_codebase`, artifact bootstrap | `meta_write_error` (present only on failure) | fleet-watch#112 |
| `search_codebase` | hits with `kind: "File"`; `label_filter: "File"` | fleet-watch#112 |
| coverage (`analyze_codebase`, `index_status`, `query_graph(graph="missed")`) | `outside_build_targets`, `feature_gated`, `unlinked_file` `{count, files}`; `pruned_dirs` `{count, dirs: [{path, reason}]}`; `cargo_attribution` `{status, detail}` | #284, #291, #292, #300, #316 |
| `analyze_codebase.lsp_status`, `lsp_resolve` | `state: "completed_unresolved"`; `server_health` `{health, message, readiness}`; `outside_targets_count`; `unlinked_file_check`; reason `lsp_workspace_load_failed` | #282, #284, #292, #315 |
| `get_impact` | `unresolved_callsites_naming_target`, `unresolved_callsites_outside_targets` | #283, #284 |
| Calls edges | `resolution_method: "receiver-type"` (Rust, Python, TypeScript), `"receiver-local-binding"` (Rust) | #283, #290 |
| graph schema | `CallSite.unresolved_reason` (`"outside_compiled_targets"`), `Function.entry_kind` | #284, #273 |
| `check_security_gates` | `report.assessment_complete` | #276 |

### Added

- Query-time staleness guard (fleet-watch#112): `search_codebase`, `get_symbol`,
  and `get_impact` now report a `graph_freshness` object (`state: "fresh" |
  "stale" | "unknown"`, `dirty_files`, `checked_files`, `commits_behind`,
  `commits_ahead`) so a silently stale graph — the working tree has moved since
  the last index — becomes a visible, reasoned-about condition instead of a
  wrong answer presented with full confidence. It rides on EVERY response these
  tools return — `symbol_not_found`, `query_failed`, and a failure to open the
  store or run the query alike, the last being exactly what an in-progress
  re-index looks like from a read tool. That is structural, not a matter of
  remembering: the receipt is stamped at each tool's single exit, so a future
  `?` anywhere in a handler body is covered by construction. The key is
  deliberately NOT `graph_state`, which
  `index_codebase` / `index_history` already use for a plain string
  (`"fresh"` / `"accepted_stale"` / `"filled_to_working_tree"`) — one key with
  two shapes would break any client that types the field once. The commit
  signal is
  bidirectional: `commits_ahead` counts commits the indexed sha has that HEAD
  does not, so a checkout BACK to an older revision is reported as stale
  instead of scoring 0 like an unmoved HEAD. Re-stats the
  `file_manifest.json` sidecar's tracked files (no re-hashing, no directory
  walk) — `O(manifested files)` `stat` calls per read-tool call, documented on
  `count_dirty` — plus one `git rev-list --left-right --count` when the indexed
  root is a git working tree. `meta.json` moves to schema 2, adding
  `commit_sha`, and is now written atomically (temp file + rename, the temp
  file named per writer) so neither a concurrent reader can be handed a torn
  sidecar nor two concurrent indexers of one `output_dir` interleave through a
  shared temp path; a schema-1 sidecar from an older index still parses, just
  without the commit signal. The write goes through the repository's existing
  `handler_util::atomic_write` rather than a local reimplementation of it, which
  restores the `fsync` a hand-rolled version had dropped — without it a crash
  between the write and the rename could publish an empty or truncated sidecar.
- `write_graph_meta` reports a failed sidecar write instead of logging it and
  carrying on (fleet-watch#112). That was defensible while the sidecar was only
  a convenience for path reconstruction; it is not, now that the staleness
  receipt reads it. On Windows a rename over a destination another process holds
  open can fail with a sharing violation, and a swallowed failure there leaves
  the PREVIOUS `meta.json` in place while the caller is told a fresh index
  completed. `index_codebase` now carries a `meta_write_error` field on the
  response when this happens; the index itself still succeeds.

- BM25 doc/prose content search (fleet-watch#112): `search_codebase` now also
  finds matches in the full text of markdown/plain-text/similar doc files —
  previously BM25 covered symbol nodes only, so a query whose only evidence
  lived in README/skill/doc prose returned nothing a code-symbol match could
  ever satisfy. Doc files (extension in a small allowlist: md, markdown, mdx,
  txt, rst, adoc; capped at 256 KiB) get a `File`-labeled hit carrying their
  content in a new, unstored `body` field, read directly from the indexed root at
  search-index build time (the parser never turns them into symbols, so the
  graph itself never stores their bytes). `label_filter: "File"` restricts a
  query to doc-content hits specifically — and requires that index, so on a
  graph built without one the filter is refused with an explanation instead of
  returning an empty result indistinguishable from "no doc matched".
  `search::build_search_index` and `bm25::build_index` now take the indexed
  codebase root as a parameter.

  Scope, stated rather than implied: doc content reaches the **lexical** half of
  hybrid retrieval only. BM25 indexes doc bodies; the TF-IDF vector index still
  covers symbol nodes exclusively, so a doc hit is fused into RRF from one
  ranking rather than two. A query that matches a doc semantically but shares no
  term with it lexically will not surface it. Extending vector indexing over doc
  bodies is deliberately left to its own change.

- Static receiver-call resolution for Rust (#283). The resolver looked the
  WHOLE callee spelling up as a symbol name, receiver included
  (`idx.by_name["self.response_of"]`), so every Rust method call made through
  a receiver was structurally unresolvable — 0 of 52 on the dy-wcet corpus —
  and `get_impact` answered "no callers" for methods called only that way. The
  receiver's type is already in the graph: a method's qualified name is
  `{impl_qn}::{method}`, so the caller's own name minus its last segment is the
  type `self` denotes. Two new `resolution_method` tiers on `Calls` edges:
  `"receiver-type"` (confidence 0.93) binds `self.m()` / `Self::m()` inside an
  `impl` method, by exact impl-type key or else the single candidate whose
  parent type matches; `"receiver-local-binding"` (0.87) binds `x.m()` where `x`
  is bound exactly once in the enclosing function by a typed parameter, a typed
  `let`, or `let x = T::assoc(..)` (the parser now records this as
  `CallSite.receiver_hint`). Two or more candidates land as ambiguous, zero as
  not found; a receiver-shaped callee never falls back to a bare-name lookup,
  which is what keeps the false-caller count at zero. Measured on dy-wcet:
  `response_of` call sites resolved 3/52 → 44/52, distinct callers found
  3/38 → 35/38, zero false positives. The 8 still unresolved bind their
  receiver with `let s = generate(&mut rng, n);`, a free-function initializer
  that carries no type name, a receiver shape the local-binding tier does not
  claim.
- The same receiver resolution for Python `self.m()` and TypeScript `this.m()`
  (#290). Both languages share Rust's `{type_qn}::{name}` method naming, so the
  `self`/`Self` tier was generalized rather than copied: the only per-language
  fact, the receiver spelling, is now a `LanguageProvider` property
  (`self_value_prefix` / `self_type_prefix`), and a language that does not opt
  in is unchanged. Python and TypeScript edges resolved this way carry
  `resolution_method: "receiver-type"`. The `"receiver-local-binding"` tier
  stays Rust-only. The accuracy gate's ground truth gains 83 same-class
  method-to-method `Calls` edges the fixtures had omitted, each checked pair for
  pair against CPython's `ast`.
- `get_impact.unresolved_callsites_naming_target` (#283, #285): the count of
  unresolved `CallSite` nodes whose callee names the target (bare, receiver, or
  qualified spelling). Previously this evidence existed only as prose inside
  `epistemic_reasons`, so an empty `callers` list could not be told apart from
  "genuinely has no callers" without parsing a sentence. When `callers` is
  empty and the count is non-zero, `next_steps` points at `lsp_resolve` /
  `analyze_codebase(lsp: true)` and `query_graph(graph="missed")`.
- Files outside every compiled Cargo target are attributed, not silently absent
  (#284). A Kani proof harness, a `fuzz/` directory excluded from the workspace,
  or any `.rs` file the walker indexes but Cargo never compiles now lands in a
  new `outside_build_targets` coverage bucket, computed from
  `cargo metadata --no-deps --offline`. The LSP pass no longer queries call
  sites in those files — no `textDocument/definition` answer is possible there —
  and counts them in a new `outside_targets_count` on `lsp_resolve` and
  `analyze_codebase.lsp_resolve`, kept out of both `failed` and `skipped`;
  `resolved + failed + skipped + outside_targets == total` holds by
  construction. Each such site is stamped `CallSite.unresolved_reason =
  "outside_compiled_targets"`, and `get_impact` reports
  `unresolved_callsites_outside_targets` with the file named in its reason.
  Measured on dy-wcet: `kani/response_bounds.rs` is attributed, `failed_count`
  drops 382 → 329 by exactly the 53 outside-target sites, and
  `get_impact(TaskSet::response_of)` reports 5 of them.
- `feature_gated` coverage bucket (#291). A module declared behind
  `#[cfg(feature = "x")]` for a feature the default build leaves off sits inside
  a compiled target's directory, so `outside_build_targets` calls it compiled;
  and when the static resolver binds every call in it, no LSP pass opens it, so
  `unlinked_file` never sees it either. On the issue's own fixture the coverage
  report came back empty. The indexer now walks each crate's module tree from
  its target entry files (`mod name;` edges), evaluates each declaration's
  `#[cfg]` against the package's default feature closure (the `features` table
  of the same `cargo metadata` call), and flags files reached only through a
  false declaration. Evaluation is three-valued: only `feature = "..."` leaves
  are decided, so `cfg(test)` or `cfg(unix)` never flag a file. Recomputed on
  every incremental pass, since editing `[features]` changes the verdict without
  touching a `.rs` file.
- `unlinked_file` coverage bucket and `unlinked_file_check` (#292).
  rust-analyzer's own `unlinked-file` diagnostic — "this file belongs to no
  crate in the crate graph", the exact condition #282 and #284 reconstruct by
  other means — is now read and cross-checked against the `cargo metadata`
  attribution. **The issue asked for the push channel
  (`textDocument/publishDiagnostics`); measurement against rust-analyzer 1.95.0
  showed that channel never carries the code — the notifications it pushes for
  the same files arrive with an empty list — while the pull request
  `textDocument/diagnostic` does return it. The pull channel is what is wired,**
  once per file the LSP pass opens, when the server advertises
  `diagnosticProvider`. `lsp_resolve` and `analyze_codebase` report
  `unlinked_file_check` `{pull_supported, files_checked, unlinked_count,
  unlinked_files: [{path, cargo_attribution}],
  linked_despite_outside_build_targets_count,
  linked_despite_outside_build_targets}` — disagreement in either direction
  stays visible. An unlinked file never fails the phase. Because only an LSP
  pass can recompute it, an incremental pass drops the bucket rather than
  carrying a stale verdict forward.
- `coverage.cargo_attribution` `{status, detail}` (#316). When `cargo metadata`
  failed (the #282 nested-workspace shape) or `cargo` was not on `PATH`, the
  target map was unknown and attributed nothing, so `outside_build_targets` and
  `feature_gated` rendered as `{count: 0, files: []}` — byte-identical to a
  clean Rust corpus. `status` is now `known`, `unknown` (with cargo's stderr,
  "cargo not found on PATH", or the parse error in `detail`), or
  `not_applicable`; a sidecar written before this release renders
  `not_recorded`, never `unknown`. `get_impact` no longer reports `exact` under
  an `unknown` status, and names the cause.
- `lsp_status.server_health` / `lsp_resolve.server_health`
  `{health, message, readiness}` (#282): rust-analyzer's own
  `experimental/serverStatus` health and message, previously discarded, plus why
  the readiness wait ended.
- Rust test and proof entry points (#273): `#[test]` and `#[kani::proof]`
  functions are recorded exactly (`Function.entry_kind`, surfaced on
  `get_processes` rows) and become process entry points. Classification needs
  the exact attribute; a test-like name alone no longer qualifies. Measured on
  dy-wcet: test processes 0 → 49, proof processes 0 → 5.
- `check_security_gates` reports `report.assessment_complete` separately from
  its zero-critical-flags verdict (#276), so an empty or unknown input, or a
  skipped check, can no longer read as a clean pass.
- `query_graph` admits schema introspection: `CALL` is classified per procedure
  against a read-only allowlist (`TABLE_INFO`, `SHOW_TABLES`) instead of being
  refused wholesale (#263). See Security for why the allowlist is per procedure.

### Changed

- The walk ingests what its blanket dot-prefix rule used to hide, and names
  every path it refuses (#300, #301). Measured on this repository before the
  change: 818 tracked files, 549 indexed, 270 absent from the manifest, none of
  them recorded anywhere, and the run reported `status: ok`. Dot-prefixed
  directories and files are now indexed (`.github`, `.claude`, …); the machine
  state the rule was there for is named explicitly instead (`.hg`, `.svn`,
  `.cache`, `.next`, `.terraform`, … alongside the existing `.venv`,
  `.gradle`, …). `bin` leaves the prune list — Cargo compiles every
  `src/bin/*.rs`, and this repository's own `src/bin` was invisible to its own
  index; a compiled binary is still rejected by extension. A directory holding
  its own `.git` is pruned by structure as `nested_repository`, which keeps
  agent worktrees from indexing a codebase once per checkout. Every prune is
  recorded in the new `coverage.pruned_dirs` with a reason from a closed set
  (`vcs`, `tool_artifact`, `dependency_or_build_dir`, `nested_repository`),
  outside the gap buckets — a declared policy is not a coverage gap, and
  counting `.git` as one would make `exact` unreachable everywhere. Expect more
  files, and more `File` nodes, on a re-index.
- `lsp_status.state == "completed_unresolved"` (#282, broadened by #315). A
  pass that resolved nothing but had sites to resolve used to report
  `completed`, the same value as "there was nothing to resolve". The rule is
  now `resolved_count == 0` with any failed, skipped **or outside-target** site.
  The last clause is #315: a `[workspace] members = []` root whose only crate is
  not a member attributed all 461 of its sites outside the targets and still
  reported bare `completed`. Any resolution at all keeps `completed`, so a Kani
  proof file beside a working crate is not by itself a failure. One rule, shared
  by `analyze_codebase.lsp_status` and the standalone `lsp_resolve`.
- `query_graph` refuses `;`-chained statements with the named reason
  `multi_statement_not_supported` instead of the engine's message under a
  generic `query_failed`; a single trailing `;` is still accepted (#261).
- Contributors: every commit must now carry a DCO `Signed-off-by:` line
  (`git commit -s`), checked on every pull request by a workflow using no
  third-party action (#310). The dependabot exemption keys on the pull
  request's GitHub login first and the author name second, so a commit merely
  authored as `dependabot[bot]` is not exempt.
- Internal refactors and CI maintenance with no behavior change: files and
  functions split under the size caps (`search/mod.rs`, `tool_schemas.rs`,
  `indexer/incremental.rs`, and others), the marketplace pin-gate scripts
  re-synced byte-for-byte with their canonical copy (#314), and GitHub Actions
  bumps.

### Fixed

- `analyze_codebase` writes the file manifest, not just `meta.json`
  (fleet-watch#112). It and `index_codebase` are documented as interchangeable
  entry points over one `output_dir`, so an analyze run on top of an earlier
  index froze that index's manifest in place and every file added afterwards was
  permanently invisible to the staleness check — the graph read `fresh` while
  missing them, with no race involved.
- `file_manifest.json` is written through the same atomic helper as `meta.json`,
  which adds the `fsync` and the per-writer temp name it previously lacked. The
  two sidecars are compared against each other, so the pairing defence was only
  as strong as the weaker of the two writes. That helper now lives in one place
  (`atomic_file`) instead of being reimplemented per call site.
- A failed sidecar write is reported by every path that writes one — the two
  bootstrap paths and `analyze_codebase` previously logged it and carried on, so
  only `index_codebase` surfaced it to the caller.

- The query-time staleness receipt no longer reports a verdict it cannot stand
  behind (fleet-watch#112). Three cases now read `"unknown"` instead: the graph
  artifact itself is gone (previously `"fresh"` alongside the tool's own
  `status: "error"`); the two sidecars do not belong to the same index; and the
  manifest is empty with no git provenance, where nothing was examined at all.
- `meta.json` and `file_manifest.json` are written in one order by both indexing
  paths — manifest first, `meta.json` last — making `meta.json` an index's
  commit point, and it now records which manifest it accompanies. The full
  re-index wrote them the other way round from the incremental path, so a read
  landing between the two paired a fresh commit sha with the previous manifest
  and called a just-rebuilt graph stale, single-process, with no concurrent
  writer involved. `meta.json` moves to schema 3; a schema-1/2 sidecar still
  parses and simply skips the pairing check.

- The LSP pass fails loudly when the language server loads no workspace
  (#282). A crate nested under a parent Cargo workspace that does not list it
  as a member makes rust-analyzer report `experimental/serverStatus`
  `health: "error"` — it loaded no crate graph. The server discarded that, sent
  every `textDocument/definition` anyway (634 of them on the reproduction), got
  `[]` for each — individually indistinguishable from a legitimately
  unresolvable call — and reported `state: "completed"`, `status: "ok"`. The
  pass is now gated on the server's health before a single request is sent:
  `lsp_status.state` is `"failed"`, the error starts with
  `lsp_workspace_load_failed:` and carries the server's own message plus the
  Cargo-level remedy (add the package to the parent's `members`, or analyze the
  workspace root), and `lsp_resolve` answers with reason
  `lsp_workspace_load_failed`. Static analysis is unaffected and the graph is
  still written. `health: "warning"` does not gate: it also accompanies zero
  resolution for reasons that are not this failure.
- A chained method call split across lines now resolves through LSP (#317).
  `lsp_position` added the byte offset of the method's name within the callee
  text to the stored column but kept the stored line; the callee text is the
  verbatim source from the call's start, so for `Task::new(..)\n    .deadline(..)`
  the request went to a column past the end of the FIRST line and silently never
  resolved. The position now advances one line per newline before the
  identifier and counts the column from the last one. Reported as a regression
  between two dy-wcet measurements; it was not one — the edges that disappeared
  had come from speculative argument sites that #294 removed by design, and
  every multi-line chain site had been unresolved in both graphs. On dy-wcet,
  13 multi-line chain sites move from unresolved to resolved, none the other
  way.
- `lsp_resolve` resolves receiver method calls at all (#267). Measured on
  dy-wcet: `resolved_count` 0 (615 failed) → 188. Three independent causes: the
  indexer hardcoded `CallSite.col` to 0 for every call site (every parser now
  emits the 0-based column LSP positions require); the column pointed at the
  receiver (`self`), which rust-analyzer resolves to the receiver's binding,
  not the method (the request now targets the method identifier); and the pass
  queried before rust-analyzer had loaded its workspace, so every request raced
  it and got `[]` (the client now waits for `experimental/serverStatus`
  `quiescent: true`, with a `workDoneProgress` quiet-window fallback for
  servers that do not send it).
- `lsp_resolve` inserted zero edges on every run (fleet-watch#18, #261): the
  definition URI was compared as an absolute percent-encoded path against node
  keys relative to the codebase root, so every lookup missed. URIs are now
  decoded (including the RFC 8089 `file://localhost/` form) and both sides
  canonicalized; definitions outside the root yield nothing rather than a wrong
  key. Unresolved sites are now selected per call site rather than per caller
  (a caller with one resolved site used to have its other nine skipped); edge
  insertion checks the exact `(rel, from, to)` triple so an interrupted or
  repeated run cannot duplicate edges; the skipped count is the identity
  `total - resolved - failed` instead of a subtraction that saturated to zero
  past the first file; and a graph indexed by an older build (no
  `CallSite.is_resolved` column, or NULL values) is migrated instead of taking
  the tool down or reporting a successful zero-site run.
- LSP no longer fabricates edges from a line collision (#271). The definition
  answer was matched onto graph nodes by (file, line) with a ±3-line fallback;
  rust-analyzer resolving a call ARGUMENT to its own parameter declaration,
  which shares a line with the enclosing method's signature, produced a false
  self-edge (`total → total`, confidence 0.9) — 11 of 119 LSP-derived edges
  (9.24%) on an external corpus. The fallback is gone, `linkSupport` is
  declared so the server can return the precise name span, and the resolved
  node's own name must match the call site's identifier.
- The LSP client's timeout is real, and it cannot mistake a server request for
  its own answer (#263, #264). The timeout was consulted between blocking reads,
  so a server that wrote a partial header and stopped held the whole indexing
  run for as long as the child lived; reads now happen on a reader thread and
  the caller waits on a channel that enforces the deadline. Responses are
  matched by `id` AND the absence of `method` — JSON-RPC's two id spaces are
  independent, so a server-initiated request sharing our id (e.g.
  `window/showMessageRequest` during the handshake) used to be taken as the
  answer. One malformed notification no longer fails a request the server
  answers a frame later; `shutdown` is bounded; a timeout is classified by an
  exact prefix rather than by the word "timeout" appearing anywhere in an error.
- `get_impact` resolves its input before answering (fleet-watch#19, #261). A
  `src/main.rs::foo` spelling (the README's own form) matched no node and
  returned empty callers labelled `epistemic: "exact"`. The target is now
  resolved the way `get_symbol` / `get_context` resolve it, a genuine miss
  returns `symbol_not_found` with suggestions, File targets
  (`get_impact("src/main.rs")`) work, and the co-change section and
  `next_steps` key off the resolved target instead of the raw input.
- `get_impact`'s `exact` requires positive evidence (#268, #299). The boundary
  was derived from the ABSENCE of known uncertainty, so a symbol whose call
  sites were never extracted had nothing unresolved and was reported complete —
  `get_impact(Response::meets)` answered `exact, 0 callers` for a method with
  eight call sites, four of them in Kani proof harnesses. `exact` now needs a
  coverage record and zero gaps across `parse_incomplete`, `skipped`,
  `quarantined`, `outside_build_targets`, `unlinked_file` and `feature_gated`,
  and any unresolved call site naming the target downgrades it too. The rule
  can only weaken a claim.
- Rust extraction, both directions (#272, #294). Calls inside a macro's
  arguments were invisible — tree-sitter does not expand macros, so
  `assert_eq!(s.slack_of(1), None)` yielded only the `assert_eq!` site; they are
  now reconstructed from the token tree for `x.m(..)`, `T::m(..)` and a method
  on a call result (`f(..).meets(..)`), with a comma between two tokens refused
  as a receiver. Conversely, a parameter or `let` binding passed as an argument
  was emitted as a speculative call site, and one named `t` collided with a real
  `tests::t` helper to report 20 callers that do not call it; names bound in the
  enclosing function (including shorthand struct-pattern fields) are no longer
  emitted.
- A struct field and a method sharing a qualified name are both kept (#269,
  #270). Node dedup was keyed on the qualified name alone, so
  `TaskSet::len` the field silently replaced `TaskSet::len()` the method, which
  was then invisible to every tool. Dedup is now keyed on (label, qualified
  name), and the label lookup records every label per name, so a legal Rust
  namespace collision (`mod foo` beside `fn foo`) is treated as ambiguous
  instead of routing an edge into whichever label was parsed last.
- `analyze_codebase` persists its coverage receipt and surfaces an optional LSP
  phase's failure in `lsp_status` instead of dropping it (#273).
- Verification tools no longer return false-clean results (#276).
  `check_security_gates` ignored unresolved imports because it queried a
  property that does not exist, and `verify_semantic_diff` counted every
  `Import` node whether or not its resolution changed. Both now read the actual
  unresolved status and propagate query errors instead of reading them as
  "nothing found"; a newly unresolved import scores at least `concerning`.
  Semantic-diff evidence is sorted before truncation, so the details returned
  are reproducible.
- `query_graph` no longer refuses queries over this schema's own `Import` table
  (#261): the keyword gate treated the node label `:Import` as the `IMPORT`
  statement. A keyword introduced by `:` or `.` is a label, relationship type or
  property, never a clause.
- `query_graph` bounds a query whose only `limit`/`order by` is inside a string
  literal or comment (#263). `WHERE n.name = 'limit'` looked like a query that
  had declared its own `LIMIT`, so none was injected and the `MATCH` ran
  unbounded; a literal `order by` advertised `order_stable` for a page with no
  ordering at all.
- Community and process lookups agree across tools (#263, #264): an empty
  community id or process name is not an answer at any of the readers, and a
  real community always wins over a degenerate one, so `get_impact`,
  `get_context`, `cluster_graph` and `query_graph` report the same membership
  for one symbol.
- A use-after-free on shutdown of the graph store (#312): the lbug `Database`
  was dropped before the `Connection` and prepared statements pointing into it,
  which a cold build on a new CI runner image turned into a deterministic
  SIGSEGV.

### Security

- The sidecar-pairing check can no longer be switched off by the sidecar
  (fleet-watch#112). It trusted a `schema_version` the sidecar declares about
  ITSELF, inside a module whose whole threat model is that the sidecar is
  attacker-writable — so a forged `meta.json` carrying nothing but a root, a
  commit sha and `"schema_version": 2` bypassed the entire round-4 pairing
  defence with one field and no race at all. The version is no longer consulted:
  a sidecar that records no manifest identity does not pair, whatever it says
  about itself. A graph indexed by an older build therefore reads as
  `"unknown"` until re-indexed — the honest answer, rather than a bypass
  reporting `"fresh"`.

- The staleness check no longer trusts `meta.json`'s `root` as a filesystem
  join base (fleet-watch#112). `root` is the base for every `stat` and the `-C`
  argument to `git`, so an unvalidated value walked straight past the manifest-key
  containment below it: `"root": "/"` with an ordinary key like `"etc/shadow"`
  resolved to an absolute system path and the dirty count reported whether that
  file matched the attacker's guess. The root must now be absolute, resolvable,
  a directory, and not one of the system paths the server already refuses
  elsewhere.

  The blacklist is matched against the CANONICAL form of each entry as well as
  its literal one. Comparing a canonicalized root against literal strings did
  not work and silently did not: on macOS `/etc`, `/tmp`, `/var` and `/home` are
  themselves symlinks, so `"root": "/etc"` canonicalized to `/private/etc` — a
  path the literal list never named — and was accepted, reopening the oracle for
  a real directory rather than the degenerate `/`.

  This is a BOUNDED mitigation, not a closure, and the residual is exactly
  this: **an attacker with write access to `output_dir` can still direct `root`
  at any real, absolute, resolvable, non-blacklisted directory on the host and
  use `dirty_files` / `state` as an existence, size and mtime oracle for files
  under it.** What no longer works is a non-existent, non-canonicalizable,
  non-directory, or blacklisted-system-root value. No on-disk sidecar can
  authenticate the root; the two real closures — a caller-supplied
  `codebase_path` on the read tools, or a signed sidecar — are a breaking
  schema change and a break of the portable-artifact use case respectively, and
  are deliberately deferred as design decisions. See the note on
  `validated_root`.
- The query-time staleness check no longer resolves `file_manifest.json` keys
  that escape the indexed root (fleet-watch#112). `Path::join` discards its base
  when handed an absolute component, so a crafted sidecar key — `/etc/shadow`,
  or a `../` traversal — was `stat`ed outside the indexed tree, and the reported
  dirty count then told the caller whether that file's mtime and size matched
  the value they had planted: an existence-and-attributes oracle over the host
  filesystem, one guess per read-tool call. Manifest keys must now be relative
  paths of ordinary components, which is what the indexer has always written;
  anything else is refused before the `stat` and counted as unverifiable rather
  than silently treated as evidence of freshness.

- `query_graph`'s read-only guarantee is enforced at two layers, because
  neither covers the other (fleet-watch#15, #261, #263). The engine's own
  `is_read_only()` now refuses database mutations however they are spelled. It
  does NOT refuse filesystem writes: lbug's read/write analyzer leaves
  `COPY … TO`, `EXPORT DATABASE`, `IMPORT DATABASE`, `ATTACH`, `DETACH` and
  `USE` at a no-op, so on a read-only handle `COPY (…) TO 'file'` executes and
  writes an attacker-named file (measured on lbug 0.19.1). The lexical gate
  therefore refuses all six — `DETACH` and `USE` passed both gates before a
  re-audit from lbug's own headers. The lexical gate and the engine are
  differential-tested against each other on quoting, escaping and comment
  grammar, so a future lbug lexer change breaks a test instead of opening a
  hole. `CALL` is admitted per procedure only: the engine classifies
  `CALL threads = 8`, a configuration write, as read-only, so relaxing the
  keyword wholesale would leave nothing standing in front of it. Queries carry
  a 30-second bound, applied before prepare so binding and planning are bounded
  too.
- Cypher injection closed on the lookups that still interpolated a caller value
  (fleet-watch#16, #261, #263). `lookup_community` / `lookup_processes` built
  `= '{node_id}'` by string formatting; the hot read lookups (community
  membership, impact-target file resolution, LSP caller-label lookup) now bind
  the value as a parameter instead of escaping it into the text. The mechanical
  guard `tests/no_naive_cypher_escape.rs` now also rejects the `CONTAINS` /
  `STARTS WITH` / `ENDS WITH` / `IN` forms, one of which the impact-target
  lookup was using.

### Dependencies

- `tree-sitter` 0.26.11 → 0.27.0 (#306), `lbug` 0.19.1 → 0.20.4 (#308), `zstd`
  0.13.3 → 0.14.0 (#307); patch bumps of `blake3`, `tantivy` and `clap` (#274,
  #313). The `graph_accuracy` gate passes on this release with all of them.
  `tree-sitter` 0.27's `Node::child_count()` returns `u32`, which only removed
  a now-redundant cast in a test. `lbug` 0.20
  reversed its Windows OpenSSL link-library naming; the Windows build and
  release workflows now provide both names.

## [0.11.1] — Ingestion must never abort on graph size

Patch: a backwards-compatible bug fix — no API change, no new feature.

### Fixed

- Ingestion no longer aborts when the graph outgrows 8 GiB: the production
  `max_db_size` default moves from the 8 GiB cap (issue #25) to lbug's own
  8 TiB VM-region ceiling — the engine's behavior when `max_db_size` is left
  unset. `max_db_size` sizes an mmap address-space reservation, not an
  allocation, so disk and memory still grow only with real data. Multi-TiB
  corpora now index to completion; operators who need a bound set
  `AP_LBUG_MAX_DB_SIZE` (validation unchanged: power of two, 8 MiB floor).

## [0.11.0] — User-controlled directory exclusion; graceful unreadable-directory degrade

Minor, not patch: this release adds backward-compatible functionality (a new
optional `exclude_dirs` parameter on two tools, plus a strictly more tolerant
walk on permission-denied subdirectories) with no breaking change to any
consumer-facing behavior.

### Added

- `exclude_dirs` (array of strings) on `index_codebase` AND `analyze_codebase`
  (#249, #250): a bare name (no path separator) prunes every matching
  directory anywhere in the tree, like the built-in skip list; an entry with a
  separator prunes exactly one subtree relative to the indexed path. Exclusion
  wins over every `dependency_scope` tier, including `full` — it is for
  directories that must never be read, not a performance prune. No glob
  support. Entries are canonicalized (`./`-prefixes, doubled and trailing
  separators), and validated at the MCP boundary: absolute paths and `..`
  components are rejected. Changing `exclude_dirs` on an existing graph
  requires `full=true` (the manifest does not capture it, same caveat as
  `dependency_scope`).
- Coverage honesty (#57 follow-through): pruned directories are reported in
  the coverage sidecar as skipped with reason `user_excluded`, unreadable ones
  with reason `unreadable`, and the response receipt carries a distinct count
  for each. Nothing is silently dropped.

### Fixed

- A single `PermissionDenied` subdirectory no longer aborts the whole index
  (#249, #250): the directory is recorded and skipped and the walk continues.
  An unreadable walk *root* stays fatal, and every other `read_dir` error
  remains fatal.
- Test-suite disk hygiene (#251): removed a `mem::forget` tempdir leak
  (`tests/uses_edges_92.rs`) and converted the last raw
  `std::env::temp_dir()`-based fixture (`tests/kotlin_ambiguous_calls.rs`) to
  the managed RAII guard from #236. A passing full `cargo test` now leaves
  zero new directories in `$TMPDIR` — leaked LadybugDB test databases were
  ~368 MB each and contributed to filling a developer machine's disk on
  2026-08-13.

### Changed

- Behavior-preserving test refactors (#250, #251): test functions across nine
  test files split to the coding-standards §4.2 50-line cap; the workspace
  bench binaries migrated to the new `IndexOptions` signature and their
  `main`s split along phase boundaries.

## [0.10.0] — Fix the MCP Registry publish path; generalize parsers to a data-driven engine

`v0.9.1`'s `server.json` shipped a 144-character `description` — the MCP
Registry rejects anything over 100 with HTTP 422, so that tag can never be
published, and `io.github.cdeust/ai-architect-mcp-codebase` has had no
registry entry at all since the rename (only the pre-rename
`io.github.cdeust/automatised-pipeline` at 0.8.4 was discoverable). This
release ships a 86-character description that satisfies both the registry
and this repo's own doc-claims gate.

Minor, not patch: this release adds backward-compatible functionality (new
parser conventions, new language coverage, full-AST persistence, registry
publish automation) with no breaking change to any consumer-facing
behavior — see "Changed" for the two candidates considered and ruled
non-breaking.

### Added
- **Data-driven `LanguageConventions` parser engine**, proven on Go (#222)
  then rolled out to Java (#223), with a generic tree-sitter tags-query
  extraction engine (#225), generic descent/heritage-hop/fieldless-fallback
  introspection (#226), and generic keyword-driven import extraction (#230).
  Elixir, Zig, and Bash complete the global structural vocabulary on the
  same engine (#231); the prior test-only structural engine is now dead
  code and was deleted (#234).
- **Full AST persistence.** `index_codebase` now persists the complete
  parsed AST per file — a parsed file is entirely indexed, not just the
  symbols the resolver currently reads (#236).
- `publish_registry` job in `release.yml`: publishing `server.json` to the
  official MCP Registry is now automated, running after the GitHub Release
  is public and every asset verified, authenticated via `mcp-publisher
  login github-oidc` (no stored credential). Computes the real
  `packages[0].fileSha256` from the verified public `.mcpb` asset at
  publish time rather than trusting the committed placeholder. A
  `workflow_dispatch` recovery path publishes an already-tagged, already
  public release — including the first publish under the current
  `io.github.cdeust/ai-architect-mcp-codebase` identity, which the registry
  has never served (#242).
- `scripts/repin_bootstrap_digests.py`: one command to re-pin
  `bin/ensure-binary.sh`'s `Cargo.toml`/`plugin.json` digest pins, applied
  automatically by the pre-commit hook — every dependency bump used to fail
  CI on "bootstrap Cargo manifest digest drifted" until re-pinned by hand
  (#237).
- `PENDING_SELF_PINS` in `scripts/marketplace_pins_self.py`: the marketplace
  pin gate's self-hosted-plugin check had no escape valve for a release
  genuinely in flight, unlike the equivalent `PENDING_PINS` valve its
  github-source sibling check already had. A release PR is structurally
  guaranteed to bump this repo's own pin ahead of its tag (the tag names the
  PR's merge commit, which does not exist until after merge), so every
  self-hosted release PR's own CI run of `check_marketplace_pins.py` failed
  with `PIN_VERSION_UNPUBLISHED` before this fix — caught while cutting this
  very release. Same audited-allowlist contract as `PENDING_PINS`: named,
  printed as a NOTICE, never silent.

### Changed
- **`get_impact` returns TypeScript `implements` clauses and heritage edges
  across every language declaring explicit `extends`/`implements`.**
  Previously these were resolved only for a subset of languages; the fix
  widens coverage without changing the response shape (#215, #221).
- **On-disk artifact directory renamed** `.automatised-pipeline` →
  `.ai-architect-mcp-codebase`, self-healing on every real touchpoint
  (`artifact_exists`, `export_artifact`, the hook-augment graph-presence
  check, `index_codebase`) so an existing install migrates transparently on
  next use — never a silent dual-read (#241, issue #195). Considered as a
  breaking change and ruled non-breaking: the migration is one-shot and
  automatic, and the compatibility-alias binary (`[[bin]] automatised-
  pipeline` in `Cargo.toml`, pre-existing from the earlier package rename)
  is untouched.
- `community.rs` split under the 500-line cap along its real seams (#240) —
  behavior-preserving.

### Fixed
- Marketplace pin gate restored to byte-identity with its canonical copy in
  `cdeust/Cortex` after the pin-gate module split (#243, #247).

### Dependencies
- `tree-sitter-rust` 0.23.3 → 0.24.2 and `tree-sitter-c` 0.23.4 → 0.24.2
  (#245, #246): grammar bumps, confirmed inert for this repo's extraction
  by a `node-types.json` diff and the accuracy gate.
- `toml` 0.8.23 → 1.1.3+spec-1.1.0 (#183); the `cargo-minor-patch` group
  (#227); the `github-actions` group (#229); `lbug` 0.18.3 → 0.19.1 (#244).
  No functional change observed in this repo's behavior.

## [0.9.1] — Release-skew fix: ship the deps/ prune the published 0.9.0 binary was missing

Issue #209: the marketplace 0.9.0 binary was built before #199 merged, so every
install downstream of that release still walked vendored dependency trees the
fix on `main` had already excluded. #209 closes that skew and ships two other
merged-pending fixes alongside it.

### Fixed

- **Elixir/Erlang `deps/` pruned from the indexer walk (#199).** `should_skip`'s
  prune list had no entry for `deps`, the standard Mix/rebar3 fetched-dependency
  directory (also used ad hoc as a vendored-packages dir). Indexing the Cortex
  repo walked into its gitignored `deps/` — 1.1 GB of vendored Python
  site-packages including numpy's C headers — flooding the log with
  duplicate-id warnings and timing out the Cortex→AP MCP client. Measured
  impact on Cortex: **20.6 minutes / 28,124 files** on the pre-fix 0.9.0
  release binary versus **43 seconds** with the fix applied. `deps` is now
  pruned under `DependencyScope::None` and descended under `Full`, with
  `test_dependency_scope_walk` extended to assert both.

- **Live-mount source symlink accepted under `AI_ARCHITECT_SOURCE_CHECKOUT=1`
  (#206, PR #208).** The marketplace-cache digest pin in `bin/ensure-binary.sh`
  died FATAL when a dev-symlink montage pointed the installed binary at a
  locally rebuilt dev binary, because the existing source-checkout escape
  hatch required `$ROOT/.git` and a marketplace cache never has one. The same
  explicit opt-in now also accepts a binary that is a symlink resolving
  (outside `$ROOT`) into its own `.git`-bearing source tree, announcing the
  resolved dev path on stderr exactly as the existing hatch does. The default
  path (opt-in unset) is unchanged: digest pin, provenance verification, and
  every other marketplace check still apply byte-for-byte. This also removes
  the operational need to run unreleased dev builds through the montage just
  to pick up merged fixes — the live-mount workaround is exactly how the
  #199/0.9.0 skew went unnoticed.

### Added

- **File-level `References_File_File` edges from markdown and shell sources
  (#205, PR #207).** Markdown and shell files were `File` nodes with no
  cross-reference extraction, so fan-in queries were blind to the doc/script
  hubs that dominate doc/script-heavy repos (undershoot by ~17x measured on
  zetetic-team-subagents). Three markdown extraction methods (inline links,
  backtick-quoted paths, bare relative-path mentions) and three shell
  extraction methods (`source`/dot-command, direct invocation, `$VAR`/`-`
  prefixed paths) are each resolved exact-match-only (no fuzzy suffix
  guessing) and emitted as `References_File_File` edges distinct from the
  code-only `Imports_File_File` edges.

### Lesson

- **A dev-symlink montage can make "43s on Cortex" describe a binary nobody
  downloaded.** The bench that motivated #199 ran against a live-mounted dev
  rebuild labeled 0.9.0, not the published 0.9.0 release artifact. Once the
  #206 interim workaround was rolled back to the release binary, the
  pre-#199 behavior resurfaced in production — the fix existed on `main` but
  had never shipped. Release-skew is now a named failure mode: a bench number
  is only evidence for the artifact it actually ran against, and cutting a
  release is the only way to close the gap between a merged fix and an
  installed one.

### Added

- **Canonical AI Architect Codebase distribution identity.** The repository,
  crates.io package, MCP Registry entry, release assets, MCPB manifest, Claude
  marketplace, Codex marketplace, and Gemini extension now converge on
  `ai-architect-mcp-codebase` / `io.github.cdeust/ai-architect-mcp-codebase`.
  The MCP invocation remains `ai-architect`, the previous
  `automatised-pipeline` executable remains available as a compatibility alias,
  and existing `.automatised-pipeline` graph artifacts remain readable without
  migration.

- **Native Codex plugin and Gemini workflows over the unchanged eight-tool
  `core` profile.** A repo marketplace now exposes an isolated
  `plugins/ai-architect-mcp-codebase` package with its own MCP manifest, while the Gemini
  extension version tracks the crate and discovers the same three portable
  skills: codebase understanding, impact analysis, and structural change-plan
  validation. Distribution tests lock versions, profile arguments, marketplace
  paths, skill parity, and the existing Claude/full project manifest.

- **Functional host migration checks.** Claude Code, Codex, Gemini, crates.io,
  and MCPB metadata now expose the same `ai-architect-mcp-codebase` identity.
  CI validates the real Claude plugin manifest, launches the shipped Claude,
  Codex, Gemini, and staged MCPB commands through initialize, tools/list, and
  health_check, and rejects a return of the retired public plugin names. The
  README documents the exact Claude tool-prefix and host migration commands.
  Fresh Claude marketplace installs fetch and checksum-verify the matching
  release binary before MCP startup, avoiding a cold Rust build inside the
  handshake timeout. They now fail fast when that release is unavailable;
  unreleased source checkouts retain the build fallback. A machine-readable
  `mcp-contract.json` publishes the Claude plugin, server key, and tool prefix,
  and the identity gate derives the prefix from the actual manifests.
  Release publication now stays draft until every archive, checksum, Sigstore
  attestation, SBOM, and MCPB has been verified; only then is it published and
  fetched once through the same anonymous URL used by the plugin bootstrap.
  The bootstrap fixes the trusted repository and signer workflow independently
  of plugin metadata, requires a GitHub CLI 2.68+ verifier with `--source-ref`, rejects
  archive link types by streaming one member into a regular file, and persists
  and rechecks the installed version and binary digest on every marketplace
  launch to detect cache corruption or partial replacement. The adjacent
  digest is not a trust boundary against an actor able to rewrite both files.

### Fixed

- **The windows-x86_64 asset ships again, and a broken windows build can no
  longer hide (issue #176).** `v0.8.2` published a windows tarball; `v0.8.3`
  did not, and the release still went green. Two independent causes had to line
  up for that: the leg builds only on a `v*` tag, so nothing tested it before a
  release existed, and it was `continue-on-error`, so its failure did not redden
  the release either. A platform disappeared and nothing announced it.

  The break itself was **`lbug` 0.15 → 0.18**. lbug 0.18's Kuzu core asks the
  linker for `ssl` and `crypto` — Unix-style names MSVC resolves to `ssl.lib`
  and `crypto.lib`, filenames no OpenSSL distribution for Windows ships (the
  runner image provides `libssl.lib`/`libcrypto.lib`), so `link.exe` failed
  `LNK1181`. No OpenSSL *crate* is involved — `cargo tree -e normal -i
  openssl-sys` matches nothing — so the dependency-graph claims in
  `ASSURANCE-CASE.md` are unaffected; this is a native-side link requirement.

  It was invisible in the obvious place to look: `Cargo.lock` did not exist at
  `v0.8.2` (added later in `73569fb`), so `git diff v0.8.2..v0.8.3 -- Cargo.lock`
  renders every package as an addition and buries the one line that moved.

  Fixed on three levels rather than one: the import libraries are copied under
  the names the linker asks for and exposed via `-L native=` (additive, unlike
  overwriting `LIB`, which would clobber the MSVC paths cc-rs discovers); a new
  `windows-build.yml` builds the target on **every pull request**, so a break
  now costs a red check instead of a published release; and the leg is promoted
  out of `continue-on-error`, with the dead `matrix.experimental` expression
  removed rather than left implying an exemption.

### Security

- **Release integrity: the provenance a release page shows, not just the one an
  API remembers (issue #158).** The attestation and SBOM jobs had been merged
  since before `v0.8.2`, but no published release carried either, because no
  tag had been cut since. Cutting one was necessary and, on its own, would not
  have been sufficient: `actions/attest-build-provenance` records the
  attestation in GitHub's attestation API and leaves **nothing on the release**,
  while OpenSSF Scorecard's `Signed-Releases` check scores by the presence of a
  release *asset* matching `*.sigstore.json` / `*.intoto.jsonl` / `*.sig` /
  `*.asc` (ossf/scorecard `docs/checks.md`, read 2026-07-28). A release cut from
  the workflow as it stood would have produced verifiable artifacts and still
  scored **0**.

  Each attest step now captures its `bundle-path` output and publishes the
  Sigstore bundle beside the artifact it signs — `<asset>.sigstore.json` for
  all four platform tarballs, the `.mcpb`, and the CycloneDX SBOM. That also
  keeps the signed provenance statement with the artifact and avoids a Rekor
  transparency-log lookup. Verification can still require network access to
  refresh Sigstore's TUF trust root on a cold cache.

- **Tag-signing is configured, and `v0.8.3` is still unsigned — stated rather
  than implied.** `v0.8.0` was annotated but unsigned and `v0.8.1`/`v0.8.2` were
  lightweight, so nothing ties a tag to the maintainer rather than to whoever can
  push; that is still true of `v0.8.3`. The repository now sets `gpg.format=ssh`,
  `tag.gpgsign=true` and `gpg.ssh.allowedSignersFile`, and the authorized key ships as
  `.github/allowed_signers` and `SECURITY.md` documents the verification —
  including why a key committed to the repository it verifies is a continuity
  guarantee rather than, by itself, an identity one. What is missing is
  `user.signingkey` on the maintainer's machine — tracked as **#174**. Earlier
  tags are **not** retro-signed: re-tagging would change objects users have
  already fetched.

### Fixed

- **`manifest.json` had been stuck at `0.8.0` for two releases.** The pin gate
  (`scripts/check_marketplace_pins.py`) covers `.claude-plugin/marketplace.json`,
  `plugin.json` and `server.json` — its `SERVER_JSON_SPLIT` class exists for
  exactly this failure — but not the root `manifest.json`, so the drift passed
  every check while shipping a wrong version inside every `.mcpb` bundle. Bumped
  with the rest; closing the gate hole is filed as **#172**, because the gate is
  a byte-identical copy of the canonical file in `cdeust/Cortex` and CI fails on
  any local divergence, so the fix has to land there first.

### Added

- **The five MCP handler modules that no test executed are now covered
  (issue #169).** Four modules behind advertised tools sat at **exactly 0%**
  line coverage and a fifth at 43.83% — together 1,242 uncovered lines, 23% of
  every uncovered line in the workspace. These are not helpers: they are the
  bodies behind `get_symbol`, `resolve_graph`, `cluster_graph`,
  `search_codebase`, `get_context`, `index_history`, `prepare_prd_input`,
  `validate_prd_against_graph`, `check_security_gates`,
  `verify_semantic_diff`, and the whole `start` → `append` → `finalize` /
  `abort` verification lifecycle. Every error arm, early return and degraded
  mode in them was unasserted.

  134 new tests (947 → **1081**), and the modules now measure:

  | Module | Lines | Missed | Line coverage |
  |---|---|---|---|
  | `verification_ops.rs` | 401 | 1 | 0.00% → **99.75%** |
  | `verification_core.rs` | 136 | 1 | 0.00% → **99.26%** |
  | `prd_handlers.rs` | 363 | 7 | 0.00% → **98.07%** |
  | `search_context_handlers.rs` | 308 | 26 | 43.83% → **91.56%** |
  | `symbol_handlers.rs` | 342 | 33 | 0.00% → **90.35%** |
  | *workspace* | 27,999 | 3,688 | 81.07% → **86.83%** (macOS); 81.56% → **87.73%** on the Linux CI runner, which is what the badge answers to |

  Two things are asserted throughout rather than "an error happened": the
  **reason code**, because an agent picks its next move from it (`no_session`,
  `already_finalized`, `unanswered_question` and `no_clarification_round` are
  four different instructions), and, wherever a handler writes, **whether the
  file appeared** — a response claiming `verified: true` while no receipt was
  persisted is the exact bug stage 2 exists to prevent.

  Found and fixed while writing them: `rel_table_triples()` is mirrored by hand
  from `graph_store`'s private `REL_TABLES` and `collect_edge_rows` swallows
  query errors by design, so a row naming a table that does not exist would
  silently drop that whole edge class from `get_symbol`'s answer. A test now
  executes every triple's own query against a real graph.

  The coverage badge moves 81% → 87% and the test badge 900+ → 1000+; the
  #161 gate is what required both.

- **Every numeric claim in the README is now machine-checked, and four of them
  were wrong (issue #161).** The coverage badge got a gate in #160; the rest of
  the README's numbers were still hand-typed, and hand-typed numbers rot. As
  measured on 2026-07-28: the tool badge advertised **24** against a registry of
  **26**, the language badge **10** against **11**, and the "hidden tools" note
  **16** against **18**. The tool *list* was worse than the count — it documented
  24 tools while the server registered 26, so `index_status` and `ingest_traces`
  were shipped but undocumented. Both are now listed. The same four counts were
  stale in `server.json`, `manifest.json`, `.claude-plugin/plugin.json`,
  `.claude-plugin/marketplace.json` and `docs/ASSURANCE-CASE.md`, none of which
  anything read; all five are now gated too.

  The test count was the one claim that held up: **947**, exactly what `cargo
  test` reports. Worth recording why it briefly looked wrong — `cargo test
  --workspace` reports **1035**, because it pulls in the benchmark and `zera`
  workspace members that the documented command does not. The gate measures the
  log of the same `cargo test` the required CI job runs, so the advertised
  number answers the command a reader is told to type.

  `scripts/check_doc_claims.py` grew from one claim to six, split at the
  measurement seam into `scripts/doc_claim_sources.py` (what the repository
  produces) and the checker (what the README asserts, and how the two are
  compared). 51 unit tests, stdlib-only, every arm exercised.

  Two comparison policies, chosen by how often the quantity moves. **Exact** for
  tool, core-profile, hidden-tool and language counts — each changes only when
  someone deliberately adds a tool or a language, which already owes a doc update
  in the same PR. **Floored** for coverage and test count, which move on nearly
  every commit: the README advertises a round value that floors the measurement,
  so the badge stays honest for a whole bucket and needs a human only when the
  true value leaves it. The test badge now reads **900+** against a measured 947.

  Counting is not enough on its own, so the gate also compares tool **names**
  against `FULL_TOOL_NAMES` — a count-only check would have been satisfied by
  editing the heading to 26 while leaving two tools undocumented — and checks
  that every path in the Repository-layout tree exists. That tree had shown
  `src/graph_store.rs`, `src/indexer.rs`, `src/resolver.rs` and
  `src/clustering.rs` as files for months after each became a directory.

  Runs in the `cargo test (graph accuracy gate)` job, which is a required status
  check on `main`.

### Changed

- **The Scale table is re-measured on the dependency actually in use (#161).**
  It cited `lbug 0.15.3` while `Cargo.toml` has depended on `0.18`. Re-running
  the `dba` agent's nine probes (`cargo test --release --test
  lbug_bulk_investigation`, 199 edges per strategy, 2026-07-28, rustc 1.95.0,
  macOS 26.5.1 arm64) confirms the ranking that drove the design and replaces
  every figure: UNWIND + typed `LogicalType::Struct` at **0.127 ms/edge** against
  **9.658** for the naive raw-string path — **76× faster**. The absolute numbers
  are not comparable to the 0.15.3 run, since both the engine and the machine
  changed, and the table now says so.

- **OpenSSF Best Practices answers, and the documents the silver criteria
  require (`.bestpractices.json`).** Every passing and silver criterion is
  answered with a status and a justification that cites a file in this
  repository or a dated measurement: 115 criteria, 94 Met / 6 Unmet / 15 N/A.
  Passing is 60 Met / 0 Unmet / 7 N/A; silver is 40 Met / 6 Unmet / 9 N/A. The
  answers are read by bestpractices.dev for project 13845, whose badge is now in
  the README badge row.

  Three documents the questionnaire asks for did not exist and were written
  rather than logged as gaps: `GOVERNANCE.md` (who decides, roles, continuity of
  access, and the contribution-licensing position), `docs/ROADMAP.md` (what the
  next year holds and what this project will not do), and
  `docs/ASSURANCE-CASE.md` (threat model, six trust boundaries, five claims each
  with its evidence and its stated limit, and a weakness-class table).
  `CONTRIBUTING.md` gained an explicit, mandatory testing policy.

  Two criteria were answered Unmet with a filed issue rather than argued
  around: the private disclosure channel SECURITY.md points at was disabled
  (#159, a passing MUST — now closed, see below), and no published release
  carries a provenance attestation, an SBOM or a signed tag (#158, the only
  silver MUST not met). Also filed: the ungated coverage measurement (#160),
  README claim drift (#161), and the three disabled repository security
  settings (#162).

- **Cross-language type-usage (`Uses`) edges for return types and type
  construction (issue #92).** The code graph now captures a function/method that
  names a type in its **return-type annotation** (`func F() OrderConfig`,
  `fn f() -> OrderConfig`, `function f(): OrderConfig`) or **constructs** it
  (Go composite literal `OrderConfig{..}`, Rust struct literal `OrderConfig{..}`,
  TS `new OrderConfig()`) as a reverse-`Uses` dependency, so
  `change_impact` / `get_impact(...).users` surfaces those callers. Previously
  `Uses_*` edges came only from **Field** type annotations and from **calls**
  whose callee resolved to a type — which is why Python (constructs via a plain
  call) passed the #64 eval's D4 row while Go/Rust/TS did not.

  Implemented once at the spec level: three new `LangSpec` data fields
  (`type_construction_kinds`, `return_type_field`, `construction_type_field`),
  consumed by one shared walker helper (`parser::spec::walkers::type_uses`) across
  the Go generic, Rust, and TypeScript lanes. The parser records the referenced
  types as `return_type` / `constructed_types` properties on Function/Method
  (new schema columns); the resolver's `resolve_callable_type_uses` binds each to
  its type node, emitting `Uses_<caller>_<Type>` edges (existing tables — TS
  classes are covered because AP already folds `Class → Struct`, so no
  `Uses_*_Class` table is needed). Adopting the feature for another language is a
  data row, not walker code: an empty slice adds no property and no edge, proven
  by every untouched language's parity suite passing unchanged.

  Closes the last open recall gap from the #64 head-to-head eval: `benchmarks/
  eval_headtohead/reproduce.sh` GRAPH recall **0.90 → 1.00** (go-D4 0.0→1.0,
  rs-D4 0.5→1.0, ts-D4 0.5→1.0; py-D4 stays 1.0), reaching parity with the Grep
  baseline. Regressions: `benchmarks/eval_headtohead/tests/recall_gaps_87.rs`
  (`gap2_*_d4_*`) and `tests/parser_fidelity.rs` (`uses92_*`).

- **Fuzzing harness for the parser (Scorecard Fuzzing).** `parse_file` is the
  tool's only untrusted-input boundary — the indexer walks a user's repository
  and hands whatever it finds to it — so a panic there is a denial of service on
  `index_codebase`: one malformed file would abort the whole indexing run.

  Two cargo-fuzz targets in `fuzz/`, both exercising all eleven languages via a
  selector byte: `parse_file` feeds lossy-converted arbitrary bytes (reaching
  UTF-8 boundary edges, which matter because the walkers slice source by AST
  byte offsets), and `parse_file_utf8` takes only valid UTF-8 so libFuzzer's
  coverage feedback can evolve inputs toward real code structure. Both assert
  the parser's stated contract rather than merely "does not panic": 1-based
  line numbers, non-inverted spans, ordered error ranges, and non-empty
  qualified names (a QN is a primary key).

  Verified by running them, not just building them: **222,531** and **278,747**
  executions with 0 crashes, 2130 coverage edges. `.github/workflows/fuzz.yml`
  keeps them alive — a 120s smoke run per PR (catches a broken harness without
  failing a PR over an unlucky search) and a 900s scheduled run for the real
  search.

  The `fuzz` crate is deliberately **excluded** from the workspace: libFuzzer
  needs nightly, and this workspace is pinned to stable 1.95.0, so the split
  keeps the targets real without making the primary build depend on nightly.
  Verified that `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo fmt --check` are unaffected.


### Fixed

- **Documentation claims that had drifted from the code.** The README advertised
  434 tests in three places against a suite that runs 947 (`cargo test`, measured
  2026-07-27), and both the README and CONTRIBUTING.md named "Rust 1.94+" while
  `rust-toolchain.toml` pins 1.95.0 for local builds, CI and releases.
  CONTRIBUTING.md referenced a `tests/integration/` directory that does not exist
  — the integration suites are `tests/*_integration.rs`. The README badge row now
  also carries the measured line coverage (81.59%, `cargo llvm-cov --workspace`,
  2026-07-27). Remaining drift is tracked in #161.

- **Parser: bound recursion depth (and input size) at the parse boundary
  (issue #148).** The scheduled 900 s `Fuzz` job OOMed (used 4136 MB > the
  4096 MB limit) — pre-existing on `main`, not introduced by any feature PR.
  Root cause: the recursive definition walkers (`walk_defs`, `walk_c_defs`,
  `walk_cpp_defs`, …) descend one stack frame per parse-tree level with **no
  depth bound**, and tree-sitter's error recovery on adversarial input produces
  pathologically deep trees — a 1 KiB run of unbalanced C punctuation parses to
  tree depth 853, a 4 KiB run to 3403 (measured), while the deepest real source
  file in the eval corpus is depth 26. On the fuzzer's platform the unbounded
  walk either overflows the stack (a local ASAN run reproduces this in
  `walk_c_defs` at a 4 KiB input) or, on a larger stack, recurses deep enough to
  exhaust the heap one frame at a time — the OOM. The existing 5 s timeout bounds
  a single parse's *time* and the new byte cap bounds its *size*, but the
  offending inputs are small (≤ 4 KiB, libFuzzer's ceiling) and parse in well
  under 5 s, so neither catches them — a **depth** bound does. `parse_file` now
  rejects any tree deeper than `MAX_TREE_DEPTH` (512, a ~20× margin over real
  code) with a clean `Err` **before** the walk, at all three entry points
  (`parse_with_spec`, `parse_shallow`, embedded re-parse). Complementary
  hardening: a byte-size cap (`MAX_PARSE_BYTES`, 1 MiB) rejects oversized inputs
  before tree-sitter — the same bound the indexer already applied on its
  file-read path, now hoisted to the parse boundary so a direct caller of
  `parse_file` gets the same defense-in-depth and the indexer references one
  canonical constant. No behavior change for real files: graph_accuracy stays
  41/41 and the largest corpus file (47 KB, depth 26) is far inside both bounds.
  The libFuzzer artifacts from the failing runs are committed as permanent
  regression seeds under `fuzz/corpus/`.

- **Rust extraction: module-scoped `impl` method QNs, and trait default-body
  call scanning (issues #130, #131).** Two defects the phase-8 LangSpec migration
  (#136) preserved for exact parity are now deliberately corrected:
  - An `impl` block nested inside a module now scopes its methods to the enclosing
    MODULE's QN, not the file path — `mod helpers { impl Inner { fn toggle } }`
    yields `src/lib.rs::helpers::Inner::toggle` with its `HasMethod` edge from
    `src/lib.rs::helpers::Inner`, the same QN the `Struct` node already declares.
    Before, the `Struct` and its `Method` disagreed on the owning type's QN, so
    `HasMethod` pointed at a QN no node owned and `get_context`/`get_impact` on a
    module-scoped type missed its methods. This is how Python and Java already
    scope nested definitions (#130). BREAKING for consumers of Rust method nodes
    in modules: re-index any Rust repository — a graph built before this release
    carries the old file-scoped method QNs.
  - A trait method with a DEFAULT body now has that body scanned for calls exactly
    as an `impl` method's body is — `fn describe(&self) -> String { String::from(..) }`
    emits a `CallSite` keyed by `Handler::describe`. A bodiless requirement
    (`fn handle(&self);`) still emits none. Before, every call a default trait
    method made was invisible and `get_impact` on a function called only from a
    trait default reported zero callers. This is the same asymmetry Swift #100
    (unscanned computed-property getters) and TypeScript #142 (unscanned
    object-literal bodies) closed, in a different grammar (#131).

- **TypeScript extraction: bare type annotations, abstract methods, and
  object-literal call scanning (issues #140, #141, #142).** Three defects the
  phase-9 LangSpec migration (#144) preserved for exact parity are now
  deliberately corrected:
  - A class field, interface property, or `const`'s `type_annotation` no longer
    carries the grammar's leading `: ` — `age: number` yields `number`, not
    `: number` — aligning TypeScript with the bare `type_field` convention the
    clike/constants/types walkers already use (#140).
  - An `abstract` method declared in an `abstract class` body
    (`abstract compute(): number;`) now emits a `Method` + `HasMethod` like a
    concrete method (bodiless, so no calls are scanned), so abstract members are
    visible in the graph — the same modeling Swift's bodiless
    `protocol_function_declaration` (#98) and Java's `abstract`
    `method_declaration` already use (#141).
  - An object-literal `const` (`const api = { get(u){ fetch(u) }, post: u => send(u) }`)
    now has its method and arrow-property bodies scanned for calls, keyed under
    the enclosing const's QN, so `fetch`/`send` produce `Calls` edges (#142).

- **C++ declaration names, and five C++ extraction-shape gaps (issues #123,
  #124). BREAKING for consumers of C++ nodes: names, labels, and qualified names
  change.** Re-index any C++ repository — a stored graph built before this
  release carries the old names and QNs.

  **Names came from the last parameter, not the declaration (#123).** The name
  search was a right-to-left DFS over the whole declarator, so it returned the
  deepest-rightmost identifier: `int freeFunction(int a, int b)` was named `b`,
  `Shape operator+(const Shape& other)` was `other`, and `void add(T item)` was
  `item`. Names now come from what the declarator BINDS, following the
  `declarator` field chain and never descending into the parameter list. This is
  the same fix #106 made for C, and it is now literally the same code: the search
  lives once in `walkers/declarator` and both C-family walkers drive it with a
  `DeclaratorNaming` row, so the skip stays data. `operator+` and `~Point` keep
  their declared spelling (an `operator_name` / `destructor_name` node IS the
  name; descending into it yielded `Point`, colliding with the constructor).

  **Enum members were dropped.** `enum Color { RED, GREEN, BLUE };` emitted a
  bare `Enum` with no members. Each member is now a `Constant` (`enum_entry=true`)
  scoped under its enum, matching the C model. `enum class` behaves identically.
  A member's name comes from its `name` field, so `GREEN = 5` resolves to `GREEN`.

  **Constructors and destructors were dropped.** In a class body the grammar
  spells them as a bare `declaration` (no return type), not a `field_declaration`,
  so the walker never saw them. They are now prototype `Method`s attached to their
  class.

  **`using X = Y;` was dropped.** An `alias_declaration` is a different node kind
  from the `using_declaration` the walker matched, so C++ type aliases were
  invisible. It is now a `TypeAlias` carrying the aliased type as
  `type_annotation` — the label this graph already uses for the same construct in
  TypeScript, Rust, and Go. `using namespace std;` remains an `Import`.

  **Data members were `Constant` + `Defines`; they are now `Field` + `HasField`**
  with a `type_annotation`, matching the flat C model. Every declarator is
  emitted, so `int a, b;` yields two fields (it previously yielded one, named `b`).
  Pointer and reference declarators unwrap to the bare member name.

  **Out-of-body definitions were scoped to the file.** `double
  geometry::Circle::area() const { … }` became a file-level `Function` named
  `area`, detached from its class. It is now a `Method` re-attached to the owner
  its qualifier names, with the matching `HasMethod` edge and `receiver_type`.

  Also fixed while in these files (boy-scout rule): a bodiless class/struct/enum
  specifier — a forward declaration such as `class Shape;` — emitted a duplicate
  node on the SAME qualified name as the real definition, spanning only the
  declaration line. A forward declaration declares no type and now contributes
  nothing, at file scope and in a class body alike. This is the guard #107 added
  for C, which C++ lacked.

  C++ ground truth: 41 nodes / 44 refs → 52 / 55. Every changed row was verified
  in both directions against an issue clause (the derivation is documented in
  `cpp_ground_truth.rs`); per-edge-kind F1 is 1.000, and `graph_accuracy` stays
  41/41.

- **A C++ function-pointer data member is a `Field`, not a method prototype
  (issue #135); the identical C defect is fixed too.** A member such as
  `void (*cb)(int);` has a `function_declarator` as its OUTERMOST declarator, so
  the prototype discriminator classified it as a `Method` (`is_prototype=true`).
  But a `pointer_declarator` sits between that `function_declarator` and the name
  (`(*cb)`), which makes it a pointer TO a function — data. Classification is now
  positional: a member is a prototype only when a `function_declarator` is reached
  with NO pointer/reference wrapper between it and the name, so `void (*cb)(int);`
  is a `Field` + `HasField` (`type_annotation="void"`) while `void f(int);` and a
  pointer-RETURN prototype (`int *g();`) stay `Method`s. **Consumer-visible for
  C:** the identical looseness misclassified a file-scope function-pointer
  variable (`int (*signal_handler)(int) = 0;`) as a prototype `Function`; the flat
  C walker does not model file-scope variables, so it now emits NOTHING for it —
  the single `Function|signal_handler#13` node and its `Defines` edge are removed
  from the C graph (`#13` was the highest `seq`, so no other QN shifts). The
  discriminator lives once in `walkers/declarator::binds_function_prototype`,
  driven by each family's `DeclaratorNaming` (`indirection_declarator_kinds`).

- **Objective-C: a typedef of an inline struct no longer drops the struct and
  its fields (issue #127).** `typedef struct Node { int v; } NodeT;` emitted ONLY
  `Constant|NodeT` — the walker never recursed the inline type in the typedef's
  `type` field, unlike the C walker (#107). It now also emits `Struct|Node` +
  `Field|v` (`HasField`), and an anonymous body (`typedef struct { int v; } T;`)
  is emitted under the alias with no duplicate `Constant`. **Consumer-visible:**
  the ObjC graph gains the previously invisible inline-struct types and fields
  (2 nodes / 2 refs on the parity corpus).

- **Objective-C: a keyword-selector method name is now the full selector
  (issue #128).** `- (int)areaWithWidth:(int)w height:(int)h;` extracted as
  `areaWithWidth` — only the first keyword — while message SENDS already
  reconstructed the full selector, an asymmetry. tree-sitter-objc shapes the
  selector as bare `identifier` keywords interleaved with `method_parameter`
  nodes (no `keyword_declarator`); the name is now reconstructed exactly as a
  send is, joining each keyword with a trailing `:` when there is at least one
  argument. **Consumer-visible/BREAKING for ObjC method names:** a keyword method
  is renamed to its full selector (`areaWithWidth:height:`) and a single-keyword
  selector keeps its colon (`shapeNamed:`); a unary selector (`start`) is
  unchanged. QNs and `HasMethod`/`Calls` edges re-key on the full selector.
  Re-index any Objective-C repository.

- **C preprocessor macros and inline struct definitions reach the graph
  (issue #107).** `#define MAX 10` and `#define SQUARE(x) ((x)*(x))` produced
  nothing at all, so macro-defined symbols were invisible to the graph and to
  cross-file resolution. They are now a `Constant` and a `Function`
  respectively — an object-like macro is a value, a function-like one is
  callable — both marked `macro=true` so a consumer can tell a preprocessor
  construct from a real C object. No `Calls` edges are invented from a macro
  body: a replacement list is unexpanded tokens, not an expression.

  A struct defined INLINE — `typedef struct { int x; } T;` or
  `struct Foo { int x; } var;` — carries its body in the outer node's `type`
  field, which the flat top-level scan never reached, so its fields were
  missing entirely. Both shapes now contribute their `Struct` and `Field`
  nodes. An anonymous body is emitted under its typedef alias (the alias is the
  only name that type has), and in that case no separate `typedef` `Constant`
  is emitted — two nodes on one qualified name would be a duplicate primary
  key.

  Guarded against the obvious over-correction: `typedef struct Point PointT;`
  REFERENCES an existing struct rather than defining one, and must not re-emit
  it. `struct_specifier` is the same node kind either way; only the presence of
  a `body` field distinguishes them. An earlier draft of this fix emitted a
  duplicate one-line `Point`, so that case is now a pinned negative control.

  `macro_object_kinds` / `macro_function_kinds` are `CFamilySpec` fields, so
  C++ and Obj-C inherit macro extraction as data when they migrate onto the
  same sub-table. Additive to the graph schema (existing labels reused); the C
  parity ground truth gained seven rows and lost none.


- **C functions are named by their declarator, not their last parameter
  (issue #106).** `int add(int a, int b)` extracted a `Function` named `b` —
  and so did its prototype. The old `find_identifier` was a LIFO stack-DFS that
  reached the parameter list before the declarator's own name and returned the
  deepest-rightmost identifier; the #60 phase-6 migration preserved it
  byte-for-byte to hold parity. Name resolution now follows the `declarator`
  field chain (pointer/array/parenthesized/function wrappers) to the identifier
  leaf and never descends into `parameters`, which is added to `CFamilySpec` as
  `parameters_field` so the skip is spec DATA that C++/ObjC inherit when they
  migrate onto the same sub-table.

  **Consumer-visible:** C `Function` names and their qualified names change
  (`app/main.c::b#3` → `app/main.c::add#3`), and call sites re-scope under the
  corrected QN. Resolver name-based lookups for C symbols were previously keyed
  on a parameter name. An already-indexed C repository keeps the old names until
  it is re-indexed; the graph is derived, so no migration exists or is needed.

  The defect was invisible on parameterless signatures (`int f(void)` always
  resolved correctly), so the regression tests pin the shapes that can observe
  it — named parameters, prototypes, pointer returns, storage-class specifiers —
  and keep `int f(void)` documented as the case that masked it. The C parity
  ground truth was updated by intent: exactly two `Function` rows and five
  `CallSite`/`Calls` QNs, every other row byte-identical.


### Added

- **Ruby support, and the shallow spec path that makes language count
  unbounded (issue #60, ADR-0056).** Adds `ShallowSpec` — a language described
  by node-kind lists and a grammar factory, nothing else — plus one generic
  walker that interprets it. Ruby is the first language added this way and the
  proof of the model: `src/parser/spec/ruby.rs` is a **data literal with no
  Rust logic at all** (no walker, no conventions impl, no trait, and no
  conditional anywhere in the extraction path that mentions Ruby), against
  143–265 lines of per-language Rust for each deep-path language. `.rb` files
  now index, yielding `Function`/`Method`/`Struct`/`CallSite` nodes and
  `Defines`/`HasMethod`/`Calls` edges.

  This closes #60's Done criterion ("one NEW language added purely via spec
  entry"), which the five ADR-0055 migrations did not: `LanguageConventions`
  declares six **required** methods, so an eleventh language could not compile
  without Rust. It also builds ADR-0055 §2's specified-but-unbuilt Tier 2.

  Two deliberate design choices, both asserted by negative tests:
  - **Shallow rows carry no visibility and no inheritance edges.** A name-case
    guess (uppercase ⇒ public) is right for Go and *wrong* for
    Java/Swift/Kotlin/C#, which carry visibility in modifier keywords; a
    plausible-but-false property is worse than an absent one (§13.1 F2).
  - **Ruby's `require` surfaces as a `Calls` edge, not a synthesised import.**
    Ruby has no import statement node — `require` is an ordinary call — so the
    graph reports what the grammar supports instead of inventing an edge.

  The walker stays free of per-language conditionals by two devices: import
  keywords are stripped by reading only **named** children (tree-sitter marks
  keywords unnamed, so no per-language keyword list is needed), and a
  grammar's callee position is named in the row (`callee_field`) rather than
  branched on — Ruby models `foo.bar` as `receiver` + `method`, so "first named
  child" would have recorded the receiver. That invariant is machine-checked:
  `shallow_walker_has_no_language_conditionals` fails if the module ever
  mentions a `Language` variant, with a companion test proving the check is not
  vacuous. This is the discipline the reference implementation lacks — measured
  at HEAD `97ce23f`, **126 of its 163 languages (77%) appear in `lang ==
  CBM_LANG_*` conditionals**, 176 sites across ~12,130 lines of extractor.

  Shallow rows get the **same** executable §8 validation as deep ones: the spec
  guard checks every Ruby node kind and field against
  tree-sitter-ruby 0.23.1's `node-types.json`, with a non-vacuity test so an
  empty registry cannot make it pass silently. No existing gate moved — five
  language parity suites, `graph_accuracy` 41/41, and `parser_fidelity` are
  unchanged, since the change is purely additive.

### Changed

- **Table-driven language specs — Rust migration (issue #60 phase 8,
  ADR-0055).** Migrated Rust off its hand-written walker onto the
  `src/parser/spec/` seam at exact parity, deleting `src/parser/rust/`
  (`mod.rs` + `extract/g1..g4.rs` + `extract/mod.rs`, 1649 LOC — the largest
  per-language walker in the tree) and the dead `src/rust_parser.rs`
  backward-compat shim. Parity is full 7-tuple node parity **in emission
  order** plus per-EdgeKind `F1 = 1.000` on Defines/HasMethod/HasField/
  HasVariant/Extends/Imports/DeriveImplements, pinned by
  `rust_parity_corpus.rs` + `rust_parity_tests.rs`: **61 nodes, 66 refs, 0
  parse errors**, captured from the hand-written walker before it was deleted.

  Rust fits none of the three existing structural shapes, so it carries a new
  `RustFamilySpec` sub-table + `rust_family` discriminator routed to
  `walkers/rust.rs` + `walkers/rust_types.rs` — the #109 (`clike`, flat C) and
  #125 (`cpp`, hybrid) precedent. `walk_defs`/`clike`/`cpp` and the seven
  languages riding them are untouched. What made Rust its own shape: derive
  attributes accumulate across siblings (and survive an intervening comment);
  an `impl` block emits no node and re-scopes its methods to the FILE, not the
  enclosing module; a trait requirement's body is unscanned while an `impl`
  method's is; `use` and `extern crate` emit *different* edge kinds from one
  language; and one call node can emit several call sites.

  Calls, imports and supertraits still ride the SHARED generic walkers. Three
  backward-compatible seam changes made that possible: `import_ref_kind` now
  receives the import statement (so `use` → `Defines` and `extern crate` →
  `Imports` can coexist), a new defaulted `extra_call_entries` hook lets one
  call node yield several call sites (the issue #87 higher-order-argument
  capture), and `WalkCtx` gained the host `file_path`. All seven prior
  languages' parity suites are unchanged. The spec guard now validates every
  `rust_family` node kind and field name against tree-sitter-rust 0.23.3's
  `node-types.json`, and was proven to fail loudly on a wrong kind before being
  reverted.

  Two pre-existing extraction behaviors are preserved deliberately (parity is
  the gate) and filed as separate behavior-changing follow-ups: an `impl` inside
  a module scopes methods to the file rather than the module (#130), and a trait
  requirement's default body is never scanned for calls (#131).
- **Table-driven language specs — TypeScript migration (issue #60 phase 9,
  ADR-0055).** Migrated TypeScript off its hand-written walker onto the
  `src/parser/spec/` seam at exact parity, deleting `src/parser/typescript/`
  (`mod.rs` + `extract/g1..g4.rs` + `extract/mod.rs`, 1089 LOC) — full 7-tuple
  node parity and per-EdgeKind `F1 = 1.000` (durable pin
  `typescript_parity_tests/`: 62 nodes, 78 refs). TypeScript is class-model-
  shaped but fits none of the existing lanes: six structural divergences the
  generic class-model arms cannot express without perturbing the six languages
  that ride them — class members are `Field`s (not `Constant`s as Java models
  them); a `variable_declarator` is polymorphic (arrow → a call-scanning
  `Function`, `const` → `Constant`, `let` → nothing); a call site emits TWO refs
  (`Defines` to the call-site node AND `Calls` to the callee); a getter/setter
  pair shares ONE QN with no dedup; a `type_alias_declaration` → `TypeAlias`;
  and a bare enum member → `Variant`. So TypeScript gets a dedicated
  `walkers/typescript/` walker driven by a new `TsFamilySpec` sub-table
  (`ts_family: Some(_)` routes `walk_defs` to it), exactly as C++ (#125) and
  Objective-C (#138) did for grammars that fit neither the flat `clike` walker
  nor the class-model arms. It is also the only language whose grammar depends
  on the file extension: a new `ts_language_by_ext` selects the `tsx` grammar
  for `.tsx`/`.jsx`/`.js`/`.mjs`/`.cjs` (JSX is only there) and `typescript` for
  `.ts`. The behavioral escape hatch lives in a `TsConventions` override (the
  recorded Risk-1 watch signal), and imports route through the generic
  `walk_imports` (edge kind `Defines`). The spec-validation guard now covers
  tree-sitter-typescript 0.23.2, including the `TsFamilySpec` node kinds. The
  schema file `lang_spec.rs` was split under the §4.1 cap: the four family
  sub-tables moved to `families.rs` (pure move, re-exported, no consumer import
  changed). Pre-existing defects in the deleted walker are preserved for parity
  and tracked separately: field/constant `type_annotation` includes the leading
  `: ` (#140), abstract method signatures inside a class body are dropped
  (#141), and object-literal method bodies are not scanned for calls (#142).
- **Generic walker split under the §4.1 size cap (issue #101, ADR-0055).**
  `src/parser/spec/walkers.rs` had grown to 880 lines across the
  Go/Python/Java/Kotlin/Swift migrations, over the 500-line hard cap. Split
  along concern boundaries into `src/parser/spec/walkers/`: `mod.rs` (WalkCtx,
  `parse_with_spec`, shared helpers, wiring), `defs.rs` (the `walk_defs`
  dispatcher + `emit_class`/`emit_def`/`emit_method_recv`/`emit_decorated`),
  `calls.rs`, `imports.rs`, `embedded.rs`, `types.rs` and `constants.rs`.
  Largest file is now 265 lines. Pure move — all 29 function bodies are
  byte-identical to the pre-split source modulo the visibility markers the
  module boundary requires and module-qualified call sites; no logic change,
  no public-contract change. Proven by the unchanged gates: five language
  exact-parity + per-EdgeKind F1 suites, `graph_accuracy` 41/41,
  `parser_fidelity`, 761 tests green.
- **Table-driven language specs — C migration (issue #60 phase 6,
  ADR-0055).** Migrated C off its hand-written walker onto the
  `src/parser/spec/` seam at exact parity, deleting `src/parser/c/`
  (`mod.rs` + `extract/g1..g2.rs` + `extract/mod.rs`, 465 LOC) — full 7-tuple
  node parity and per-EdgeKind `F1 = 1.000` (durable pin `c_parity_tests.rs`:
  37 nodes, 37 refs). C is the first **flat C-family** language: it does not fit
  the class-body-recursion model the generic walkers were built around (structs
  carry *fields*, not methods; names hide under wrapped declarators; enum members
  and typedefs are `Constant`s; prototypes are filtered `declaration`s;
  preprocessor conditionals wrap declarations). Rather than shoehorn it, C
  introduces a shared flat walker `walkers/clike.rs` driven by a new
  `CFamilySpec` sub-table (`c_family: Some(_)` routes `walk_defs` to it) — the
  reusable abstraction the future C++/ObjC migrations land on as two more data
  rows. The C-specific behavior lives in a `CConventions` override (147 code LOC,
  the recorded Risk-1 watch signal — in the Go–Java band): `#include` shaping,
  member-access callee extraction (`a->b`/`a.b` → `b`), and the `#{seq}` QN.
  `clike.rs` itself (≈290 code LOC) is **shared C-family infrastructure**, not a
  per-language override, and is the honest ADR Risk-#1 signal that C-family
  grammars need a structural (not just behavioral) escape hatch. The
  spec-validation guard now covers tree-sitter-c 0.23.4, including the
  `CFamilySpec` node kinds. Pre-existing defects in the deleted walker are
  preserved for parity and tracked separately: function/prototype names resolving
  to the last parameter (#106) and unextracted `#define` macros / inline struct
  bodies (#107).
- **Table-driven language specs — Swift migration (issue #60 phase 5,
  ADR-0055).** Migrated Swift off its hand-written walker onto the
  `src/parser/spec/` seam at exact parity, deleting `src/parser/swift/`
  (`mod.rs` + `extract/g1..g2.rs`, 523 LOC) — full 7-tuple node parity and
  per-EdgeKind `F1 = 1.000` (durable pin `swift_parity_tests.rs`: 51 nodes,
  51 refs). Swift's grammar diverges structurally from the JVM family and is
  handled by gated spec data plus a `SwiftConventions` override (184 code LOC,
  the recorded Risk-1 watch signal — between Kotlin's 173 and Python's 228):
  the `class_declaration` umbrella (class/struct/actor/enum/extension) refined
  by the `declaration_kind` field (`refine_class_label`), extensions marked
  `is_extension` with no conformance edges (`class_inheritance`);
  `init`/`deinit`/`subscript` routed through `function_node_kinds` with
  synthetic names + `member_kind` (`def_name`/`function_props`), the
  field-less subscript body reached via `function_body_kinds:
  ["computed_property"]`; `enum_entry` → `Variant` with `Defines`/`internal`
  and multi-name binding (`variant_edge_kind`/`variant_visibility`);
  `property`/`typealias` → `Constant` (`member_constants`). The generic walkers
  gained backward-compatible hooks (`def_name`, `variant_edge_kind`,
  `variant_visibility`), a multi-name `emit_variant`, and a body-field-first
  `call_scan_of` that confines the whole-node call-scan fallback to grammars
  with no named body field. The spec-validation guard now covers
  tree-sitter-swift 0.7.3 (ABI-15, unbumped). Pre-existing extraction gaps in
  the deleted walker are preserved for parity and tracked separately: no
  conformance/inheritance edges (#97), dropped protocol requirements (#98), the
  `Defines`/`internal` enum-member model (#99), and unscanned computed-property
  getter calls (#100).
- **Table-driven language specs — Kotlin migration (issue #60 phase 4,
  ADR-0055; PR #95).** Migrated Kotlin off its hand-written walker
  (`src/parser/kotlin/`) onto the spec seam at exact parity. Kotlin's
  ground-up `tree-sitter-kotlin-ng` grammar diverges from Java: one
  `class_declaration` kind for interface/enum/class disambiguated by content
  (`refine_class_label`), child-node bodies rather than a `body` field
  (`class_body_kinds`/`function_body_kinds`), a single supertype list →
  `Extends` (`class_inheritance`), `enum_entry` → `Constant` marked
  `enum_entry=true` (`member_constant`/`member_constants`), node-based
  visibility (`node_visibility`), and navigation-tail call callees (#29).
- **Kotlin `property_declaration` names extracted (issue #93, PR #96).** Class
  `val`/`var` and top-level vals — whose name is nested under a
  `variable_declaration` child below the direct-child identifier scan — are now
  emitted as `Constant`s. `member_constant` became `member_constants` (returns
  a `Vec`, supporting destructuring `val (a, b)`); the generic
  `emit_member_constant` iterates it.
- **Table-driven language specs — Java migration (issue #60 phase 3,
  ADR-0055).** Migrated Java off its hand-written walker onto the
  `src/parser/spec/` seam at exact parity, deleting `src/parser/java/`
  (`mod.rs` + `extract/g1..g2.rs`, ~440 LOC). Java is the first migrated
  language carrying the full OO spread the spec model must express as data:
  interfaces and annotations (→ `Trait`), enums with constants (→ `Enum` +
  `Variant`/`HasVariant`), records (→ `Struct`), and class-member fields (→
  `Constant`). The generic walkers gained five (empty-for-Go/Python) spec
  slices for that spread — `interface_node_kinds`, `enum_node_kinds`,
  `variant_node_kinds`, `variable_field_kinds`, `body_wrapper_kinds` (plus
  `variable_declarator_kind`) — so the class emitter now maps a node kind to a
  Struct/Trait/Enum label, recurses transparently through wrapper members
  (Java's `enum_body_declarations`), and emits enum variants and member-field
  constants, all as data-gated arms (no bespoke walker). The two genuinely
  *behavioral* divergences live in a `JavaConventions` override (152 code LOC
  vs Go's 112 and Python's 248 — the recorded Risk-1 watch signal: between the
  two, richer than Go but simpler than Python, the data/behavior split held):
  modifier-keyword visibility (`public`/`private`/`protected`, read from the
  node, not the name — the generic `node_visibility` hook, defaulting to the
  name-based rule) and a SPLIT inheritance model (`extends` one superclass →
  `bases`/`Extends` vs `implements` an interface list → `implements`/
  `Implements`, via the new `class_inheritance` conventions hook whose default
  reproduces Python's single-list `bases`/`Extends`). Per-EdgeKind parity held
  at F1 = 1.000 (Defines/HasMethod/Imports/Calls/Extends/Implements/HasVariant)
  against a full-7-tuple committed parity test that is the pre-migration
  walker's exact output; Go's and Python's parity are unchanged. The spec-
  validation guard now covers tree-sitter-java too. No graph-schema, MCP-API,
  or consumer change; the remaining seven languages stay on their hand-written
  walkers (strangler-fig, one language per step).
- **Table-driven language specs — Python migration (issue #60 phase 2,
  ADR-0055).** Migrated Python off its hand-written walker onto the
  `src/parser/spec/` seam at exact parity, deleting `src/parser/python/`
  (`mod.rs` + `extract/g1..g3.rs`, ~780 LOC). Python is the ADR's "richer-than-
  CBM" language (Risk #1): its underscore visibility, `UPPER_SNAKE` constant
  filter, `is_async`/decorator properties, `@property`/`@setter` QN dedup, and
  three-kind import structure (import / from / `__future__`, with dotted /
  aliased / wildcard children) all live in a `PythonConventions` behavioral
  override (248 code LOC vs Go's 112 — the recorded Risk-1 watch signal: ~2.2×,
  richer but still one behavioral trait through the shared walkers, no bespoke
  walker), while the structural node kinds are a data row. The generic walkers gained context-based methods
  (a free-function node inside a class body is a method), class-body recursion
  with base-class `Extends`, decorator unwrapping, and a field-based constant
  path — each gated by (empty-for-Go) spec slices, so no per-language walker is
  added. Per-EdgeKind parity held at F1 = 1.000 (Nodes/Defines/HasMethod/
  Imports/Calls/Extends) across all 41 `graph_accuracy` Python fixtures plus a
  full-tuple committed parity test; Go's parity is unchanged. The spec-
  validation guard now covers Python's grammar too. No graph-schema, MCP-API,
  or consumer change. The phase-1 `walk_value_decl` `||`→`&&` equivalent-mutant
  note is removed: Python's `UPPER_SNAKE` filter makes that guard observable, so
  the mutant is now killed by a lowercase-module-assignment negative assertion.
- **Table-driven language specs — scaffold + Go migration (issue #60 phase 1,
  ADR-0055).** Introduced `src/parser/spec/`: a `LangSpec` data row (structural
  node kinds per concern + grammar factory + embedded list), a
  `LanguageConventions` behavioral trait, a registry, and generic walkers
  (`walk_defs`/`walk_calls`/`walk_imports`/`walk_embedded`) that produce the
  unchanged `ParseResult`/`ExtractedNode`/`ExtractedRef` contract. Go is the
  first language migrated: its hand-written `src/parser/go/extract/*.rs` walkers
  are deleted and replaced by one spec row plus two Go-specific predicates, at
  exact per-EdgeKind parity (Defines/HasMethod/HasField/Imports/Calls F1 =
  1.000, unchanged). A spec-validation guard loads each grammar's
  `node-types.json` and asserts every node-kind string in every spec is real,
  turning a stale row from a silent symbol-drop into a loud test failure. No
  graph-schema, MCP-API, or consumer change; the other nine languages stay on
  their hand-written walkers (strangler-fig, one language per step).

### Added

- **Falsifiable head-to-head evaluation — graph tools vs Grep/Glob/Read baseline
  (issue #64).** New `benchmarks/eval_headtohead/` benchmark crate: a
  **pre-registered** (`PRE_REGISTRATION.md`, committed before execution),
  two-condition evaluation answering the same 20 questions (5 capability
  dimensions × 4 languages: Python, TypeScript, Go, Rust) with (a) AP graph tools
  and (b) a competent `Grep`/`Glob`/`Read` baseline, over a committed,
  content-hashed corpus. Drives the real library (index → resolve → cluster →
  search / impact / processes / query). Reports precision / recall / F1 (vs a
  ground truth authored into the corpus), token proxy, and tool-call count —
  **each with dispersion** (mean ± sample stdev), aggregate and per-dimension /
  per-language. Published numbers (`results.json`, `raw_results.json`,
  regenerable by `reproduce.sh`, no network / no API key): graph precision
  **1.00 ± 0.00** vs baseline 0.65; **17.4×** fewer tokens; **5.2×** fewer tool
  calls. Pre-registered H1/H2/H3 **SUPPORTED**; **H4 (recall) FALSIFIED** and
  reported as such (graph recall 0.83 vs 1.00 — AP misses a Go entry point, some
  cross-language type-usage edges, and a Rust higher-order call; the four lost
  questions are in `raw_results.json`). The blinded LLM-as-a-Judge answer-quality
  leg is **config-gated** (`AP_EVAL_JUDGE_CMD`); its absence is reported loudly,
  never silently stubbed, and its blinding + un-blinding are unit-tested with a
  mock judge. README gains a "Falsifiable evidence" section tracing every claim
  to a `results.json` field (§8). Timing stays out of CI (issue #74 lesson);
  `cargo test` gates only harness correctness + determinism.
- **Infrastructure-as-code indexing (issue #63).** The indexer now maps a
  repository's deployment surface into the graph as first-class material, in a
  post-pass that mirrors `light_link` (runs after every File node exists; never
  touches File nodes). New `src/indexer/iac/` module (dependency-free parsers —
  no new YAML crate; a hand scan gives precise parse-gap control, matching
  DeusData/codebase-memory-mcp `pass_infrascan.c` / `pass_k8s.c`):
  - **Dockerfiles** → an `IacResource` (kind `Dockerfile`) carrying base image,
    stages, exposed ports, entrypoint/cmd, workdir; `IacImage` nodes per `FROM`
    image; `COPY`/`ADD` local sources linked to their `File`.
  - **Kubernetes manifests** → one `IacResource` per `---`-separated document
    (`apiVersion`/`kind`/`name`/`namespace`), container `image:` as `IacImage`,
    and heuristic `ConfigMap`/`Secret`/`Service` name-matches + image→Dockerfile
    directory matches as reference edges.
  - **Kustomize overlays** → an `IacModule` per `kustomization.yaml`, with
    `IMPORTS` edges to referenced resources/bases/patches (overlay→base as
    module→module, resource paths as module→File).
  - All reference edges carry `(confidence, resolution_method)`: heuristic
    resolutions are `< 1.0` and unresolved references produce no edge (misses
    reported, not faked). `get_impact`/`change_impact` traverse them (the
    `Imports_*` naming plugs into the existing reverse-dependency walker).
  - Multi-document YAML is enumerated; Helm-templated (`{{ }}`) and malformed
    manifests are recorded as `parse_incomplete` gaps in the `#57` coverage
    sidecar (visible in `query_graph(graph="missed")`), never silently dropped.
  - Integrated with incremental re-indexing (`#62`): IaC nodes are
    `<file-rel>::`-prefixed, so editing one manifest re-processes only that file
    via the existing per-file symbol purge; cross-file edges target stable File
    nodes so an unchanged referencer's edge is never collaterally dropped.
  - Fixed a latent `get_impact` bug surfaced by this work: the reverse walker
    bound `qualified_name` on every edge endpoint, which raised a hard lbug
    Binder exception (silently dropping the whole query) for labels lacking that
    column — so File-targeted/-sourced dependents, including plain
    `Imports_File_File` light-links, never surfaced. The walker now gates the
    `qualified_name` reference on `graph_store::label_has_qualified_name`.

- **Team-shared graph artifact (issue #55).** `index_codebase` gains three
  optional booleans (all default `false`, so existing behavior and the
  `core`/`core8` profiles are unchanged):
  - `export_artifact` — after a successful index, write a `tar → zstd`
    snapshot of the graph to `<path>/.automatised-pipeline/graph.zst` plus a
    `graph.meta.json` sidecar (schema version, git sha, tool version,
    node/edge counts), and append a `.gitattributes`
    `.automatised-pipeline/graph.zst binary merge=ours` entry so the committed
    blob never produces merge conflicts. Single high-compression tier (zstd-9);
    the reference's fast zstd-3 tier is deferred until AP grows a file watcher
    to call it (no caller-less code). Export failure is logged but does not fail
    the index.
  - `bootstrap` — when there is no local graph but a committed artifact is
    present, decompress the snapshot instead of cold-indexing so a fresh clone
    skips the full index. **Staleness contract:** the artifact's git sha is
    compared with the repo's current HEAD — (a) equal → import; (b) different →
    by default REFUSE and run a full index, logging how many commits behind the
    artifact is (`git rev-list --count`) and returning a `bootstrap_skipped`
    object; (c) with `accept_stale: true` → import anyway and return a
    `stale_artifact` `{artifact_sha, head_sha, commits_behind}` report so a
    stale graph can never be mistaken for a fresh one. Import failure falls back
    to a full index explicitly (logged), never a silent partial graph.
  - `accept_stale` — opt in to importing a stale snapshot (see above).
  - New `src/artifact.rs` (infrastructure module; std + `tar` + `zstd` +
    `serde` only, no `lbug`/`GraphStore` coupling). Decompression is capped at
    64 GiB and `tar` unpack rejects `..`/absolute paths, so a malicious
    committed artifact cannot path-traverse or exhaust disk on bootstrap. The
    sidecar `commit` field is validated as a hex sha before it reaches
    `git rev-list` (arg-injection guard). Reference shape:
    DeusData/codebase-memory-mcp `src/pipeline/artifact.c`.
  - Integration test `tests/artifact_bootstrap.rs` (fresh-clone import matches
    the cold index query-for-query; staleness computation) plus handler tests
    `src/main.rs::artifact_bootstrap_tests` for the refuse-and-reindex (b) and
    accept-stale (c) paths.
  - **Known gap (tracked, not silent):** the reference's *incremental fill after
    import* is blocked on AP having no incremental (changed-files-only) indexer;
    filed as #62. Until then, a stale artifact triggers a full re-index (or an
    explicit `accept_stale` import), which is correct but not yet optimal.

### Security

- **A private disclosure path that survives a settings change (issue #159).**
  SECURITY.md named exactly one way to report a vulnerability — GitHub's
  private advisory form — and that feature was switched off, so a reporter
  without write access who followed the instructions arrived at a page they
  could not use. Private advisories are now enabled
  (`gh api repos/cdeust/ai-architect-mcp-codebase/private-vulnerability-reporting`
  → `{"enabled":true}`), and SECURITY.md names a **fallback that does not
  depend on a GitHub feature flag**: `hello@ai-architect.tools`, under the
  same per-severity SLA. The disclosure timeline accepts either channel.

  No OpenPGP key is advertised alongside the address. Publishing one the
  maintainer cannot reliably decrypt with would turn a working plaintext
  channel into a broken encrypted one, so the document says so explicitly
  rather than leaving a reporter to guess.

  `.bestpractices.json`: `vulnerability_report_private` cites the fallback,
  and `achieve_passing` no longer names it as the outstanding exception —
  every passing-level criterion is now Met or N/A. The criterion tally in
  the entry above was corrected with it (93/7 → 94/6 overall, and the
  passing tier from 59 Met / 1 Unmet to 60 Met / 0 Unmet).

- **GitHub-native supply-chain controls enabled on the repository (issue
  #162).** Three controls that were switched off are now on, each verified
  through the API rather than the settings page:
  - **Secret scanning** and **push protection** — the previous evidence for
    "no leaked credential" was a manual scan run on one day, which is a
    snapshot. Push protection is the standing control: it rejects the push
    that would introduce a secret, instead of finding it afterwards. The
    first full-history scan raised **zero** alerts.
  - **Dependabot alerts** and **Dependabot security updates** — the daily
    `cargo audit` / `cargo deny` job stays the load-bearing advisory gate,
    but it only runs on its schedule; GitHub's security-update PRs are
    raised as soon as an advisory is published. Alerts had to be enabled
    first: security updates depend on them and the endpoint returned 404.

  Verification (`gh api repos/cdeust/ai-architect-mcp-codebase --jq
  '.security_and_analysis'`): `secret_scanning`,
  `secret_scanning_push_protection` and `dependabot_security_updates` all
  report `enabled`. `.bestpractices.json` drops the two
  "currently disabled" disclosure clauses this closed, in
  `no_leaked_credentials` and `dependency_monitoring`.

  Not enabled, and not claimed: `secret_scanning_non_provider_patterns` and
  `secret_scanning_validity_checks`. Validity checks transmit candidate
  secrets to the issuing provider to test whether they are live, which is a
  different privacy posture than this repository documents, and neither is
  part of issue #162's acceptance criteria.

### Added

- **A coverage floor that actually fails the build, and a README badge that
  cannot lie about it (issue #160).** Statement coverage was measured but
  ungated: the figure came from a hand-run command, so a change dropping it
  to 74% would have merged green while `.bestpractices.json` went on
  answering the OpenSSF silver criterion `test_statement_coverage80` **Met**.

  New CI job `cargo llvm-cov (80% line floor)` runs `cargo llvm-cov
  --workspace` on every pull request and push to main and fails below
  `--fail-under-lines 80`. It is a required status check on `main`. 80 is not
  a tuning knob — it is the threshold the OpenSSF criterion names.

  New gate `scripts/check_doc_claims.py` (stdlib-only, 20 unit tests covering
  every arm) compares the README's advertised coverage against what that job
  just measured, and fails when the badge **overstates**. It caught the badge
  on its first run: advertised 81.59%, measured 81.56%.

  The badge is now required to be a **whole** percentage. Pinning it at
  81.56 would have been exact for one commit and overstated by the next
  hundredth-point dip — a gate that reddens for no reason gets disabled. A
  whole-percent badge is honest for a full point of movement, which is also
  where the one-point staleness tolerance comes from rather than being picked.
  Badge now reads **81%** against a measured 81.56%.

  Worth recording: the same workspace measures **81.07%** on macOS aarch64 and
  **81.56%** on the Linux CI runner. The advertised number is the Linux CI
  figure, because that is the one a gate re-measures on every change.

## [0.8.2] — First complete four-platform release (Windows asset ships)

A CI-only patch release (PR #52). No library or server code changes.

### Fixed

- **Windows tarball step no longer exits 127 on a missing `shasum`
  (PR #52).** The v0.8.1 windows leg built `automatised-pipeline.exe`
  (49m53s, lbug's C++ core compiling cleanly under MSVC) and then died
  in *Package tarball + sha256*: contrary to the step's comment, Git
  Bash on `windows-latest` ships no `shasum`. The step now prefers
  `sha256sum` (coreutils — Linux and Git Bash) and falls back to
  `shasum` (macOS). This was the last failing step on the Windows leg,
  making this tag the first expected to publish all four platform
  assets: macos-aarch64, linux-x86_64, linux-aarch64, windows-x86_64.
- **`server.json` mcpb `fileSha256` verified against the published
  asset** (pattern established in 0.8.1: placeholder in the release PR,
  pinned post-release from the `.mcpb.sha256` asset).

## [0.8.1] — Fix: Windows release build past zstd-sys

A CI-only patch release (PR #50). No library or server code changes.

### Fixed

- **Windows release leg no longer dies in `zstd-sys` (PR #50).** The
  release workflow set `ZSTD_SYS_USE_PKG_CONFIG=1` at the job level, so
  it reached all four matrix legs. When that variable is set, zstd-sys
  *requires* pkg-config and panics if it is missing (`build.rs:60`,
  exit 101) instead of building its vendored static zstd —
  `windows-latest` ships without pkg-config, so the v0.8.0 run lost its
  Windows asset (run 29975233898). The variable only exists to fix the
  Linux-specific duplicate-libzstd-symbol conflict between zstd-sys and
  lbug under rust-lld; it now lives on a Linux-gated Build step and is
  genuinely unset (not empty) on macOS/Windows, whose legs build the
  vendored static zstd as zstd-sys intends.
- **`server.json` points at the v0.8.1 `.mcpb` with a verified
  `fileSha256`.** The 0.8.0 entry carried a stale hash that did not
  match the released bundle (the hash is only knowable after the
  release workflow packs the asset; it is now corrected post-release
  against the published `.mcpb.sha256`).

## [0.8.0] — Distribution: core tool profile, crates.io, cross-host installs

A distribution-focused release (PRs #47, #48): the server now ships a
lean 8-tool profile for outside agents, publishes to crates.io as
`ai-architect-mcp`, adds an experimental Windows release build, and
documents installation on every major MCP host.

### Added

- **`core|full` tool profiles — 8 agent-facing tools behind `--profile
  core` (PR #48).** The server registers 24 MCP tools, but half are
  internal pipeline stages (finding → PRD vocabulary) that only the
  ai-architect orchestrator calls. The `core` profile exposes the 8 an
  outside agent needs: `health_check`, `analyze_codebase`,
  `search_codebase`, `get_context`, `get_symbol`, `get_impact`,
  `query_graph`, `detect_changes`. Selection: `--profile core|full` flag
  beats the `AP_PROFILE` env var; the default remains `full` (shrinking
  the default tool surface is a breaking change reserved for the next
  major bump). A tool hidden by the profile is indistinguishable from a
  nonexistent tool, and `health_check` derives its tool count from the
  active profile's registry.
- **crates.io publication as `ai-architect-mcp` (PR #48).** Adds the
  registry metadata (license, repository, homepage, readme, 5 keywords,
  categories) and whitelist packaging (`/src/**`, `Cargo.toml`,
  `README.md`, `LICENSE` — anchored patterns keep benches/corpora,
  stages/, test fixtures, and CI config out: 110 files, 310 KiB
  compressed). The installed binary stays `automatised-pipeline`;
  `cargo install ai-architect-mcp` is now the cross-host install path.
- **Experimental `windows-x86_64` leg in the release build matrix
  (PR #48).** Marked `continue-on-error` so a Windows toolchain breakage
  never blocks the macOS/Linux release; not bundled into the `.mcpb`
  (Claude Desktop's installer contract only resolves macos/linux
  layouts). Promote to a required leg once a tagged release produces a
  working artifact.
- **Cross-host install docs.** README section covering Gemini CLI
  (including extension install via the new `gemini-extension.json`),
  OpenAI Codex CLI, Cursor, Windsurf, and VS Code, all on the `core`
  profile. The registry-ownership line (`mcp-name:
  io.github.cdeust/ai-architect-mcp-codebase`) is now visible README text in
  a Registry section, not an HTML comment only.

### Fixed

- **Registry metadata (PR #47).** LICENSE is verbatim MIT again — the
  descriptive preamble and algorithm-attribution note moved to the README
  license section, so GitHub licensee (and every directory reading the
  GitHub license API) detects MIT instead of NOASSERTION. `server.json`
  migrated to the 2025-12-11 registry schema
  (`registryType`/`fileSha256`/`websiteUrl`) required by mcp-publisher,
  and now carries the real `.mcpb` sha256 instead of a placeholder.

## [0.7.0] — Resolver correctness overhaul

Five defect clusters in the resolution pipeline, found by auditing an
anomalously low `resolution_rate` (0.23) on a large Kotlin/Android codebase
and filed as issues #28–#32; fixed in PRs #33, #34, #35, #37, #41 (plus
cleanups #38, #39). Also ships the workspace-wide clippy cleanup and its CI
enforcement gate (issues #40, #42; PRs #43, #45).

### Changed

- Workspace-wide `cargo clippy --all-targets -- -D warnings` is now clean —
  38 pre-existing violations fixed across `ai-architect-mcp` (lib, bin, test
  binaries) and `benches/harness`, including parameter-object extractions for
  `dfs_iterative`/`build_report`/`resolve_one_implements` and named type
  aliases replacing nested-tuple soups; behavior-preserving, zero test
  assertions modified (#40, #42, PRs #43, #45).
- New CI job enforces clippy `-D warnings` (workspace-wide) + `cargo fmt
  --check` on every PR, closing the enforcement gap that let the backlog
  accumulate (#42, PR #45).

### Fixed

- **`resolution_rate` is now arithmetically sound and idempotent (issue
  #28, PR #33).** Previously it could exceed 1.0 (macro resolutions entered
  the numerator but never the denominator; the Uses phase counted Field
  rows against per-type-identifier edges), collapsed toward 0 on a second
  `resolve_graph` run over the same graph (already-persisted edges were
  skipped as duplicates instead of counted as resolved), and
  `resolve_extends` counted failed inserts as successes. Every phase now
  reports through a uniform counting contract (`resolved + unresolved ==
  total_refs`, enforced by a debug assertion), `EdgeBuffer` distinguishes
  inserted / already-persisted / duplicate-in-run, and extends edges route
  through the shared buffer. **Rates reported by earlier versions are not
  comparable to 0.7.0 rates.**
- **One evidence-based ambiguity policy for all resolution paths (issue
  #30, PR #34).** Whether a callee resolved — and with what confidence —
  used to depend on its surface spelling: unqualified ambiguous callees
  were dropped as `"no target found"` while qualified ones silently took
  `candidates[0]` (indexing-order-dependent) with confidence 1.0. The new
  `ambiguity_policy` module is the single decision point, with confidence
  monotone in evidence strength (UniqueGlobal 0.95 > ImportMatch 0.90 >
  SameFileUnique 0.85 > PackageProximity 0.70). Genuinely ambiguous
  references are recorded as `ambiguous (N candidates)` — never guessed,
  never mislabeled. A deterministic-tiebreak variant was measured against
  the ground-truth accuracy fixtures, found to regress Python Calls F1
  (1.0 → 0.5), and removed (PR #38).
- **Kotlin import extraction actually works (issues #29/#31, PRs #35/#37).**
  The parser queried a tree-sitter node kind (`import_header`) that does
  not exist in the pinned `tree-sitter-kotlin-ng` v1.1.0 grammar (real
  kind: `import`), so no Kotlin import had ever been extracted — graphs
  built by earlier versions have no Kotlin import edges at all.
- **Kotlin ambiguous unqualified calls resolve via import and package
  evidence (issue #29, PR #37).** The parser now preserves real
  package/object qualifiers (`com.foo.bar.process`, `Utils.process`) while
  discarding value receivers (`viewModel.load`) so they structurally cannot
  false-match; a two-pass evidence strategy (package-keyed, then
  file-based) feeds the ambiguity policy without touching File-node
  linkage.
- **JVM/Android ecosystem imports classify as external (issue #31, PR
  #35).** The Kotlin external-prefix list covered only
  `kotlin/kotlinx/java/javax/jakarta`; `androidx.*`, `com.google.*`,
  `retrofit2.*`, `okhttp3.*` and the rest of the well-known ecosystem were
  mislabeled `"no target found in graph"`. Externally-classified references
  are also now filtered out of cross-repo bridge candidate counting (two
  independent defenses).

### Changed

- **`resolve_calls` decomposed along its concerns (issue #32, PR #41)** —
  resolution, edge-kind reclassification, validation, and metric counting
  are separately readable/testable stages instead of one 80-line loop.

### Fixed — infrastructure and `prepare_prd_input` grounding

- **Production `GraphStore` opens no longer reserve lbug's unbounded 8 TiB
  default per instance (issue #25).** PR #24 (issue #21) bounded
  `max_db_size` for `cargo test` only, via `AP_LBUG_TEST_MAX_DB_SIZE` in
  `.cargo/config.toml`; production code paths still resolved through
  `SystemConfig::default()`'s sentinel, which lbug's C++ core substitutes
  with `DEFAULT_VM_REGION_MAX_SIZE = 1 << 43` (8 TiB) per `Database::new`.
  With `graph_cache::MAX_CACHED_GRAPHS = 8` entries live in the read-path
  cache at once, that was a 64 TiB worst-case virtual-address reservation.
  Fix: `graph_store::system_config()` now resolves a new
  `AP_LBUG_MAX_DB_SIZE` production override (falling back to a measured 8
  GiB default — see the README's "Configuration — `max_db_size`" section
  for the full measurement table and sizing derivation) when the test-only
  var is absent, validating either var (power of two, ≥ 8 MiB per lbug's
  `BufferManager::verifySizeParams`) with an actionable error rather than a
  silent fallback. Worst case at the cache cap: 8 × 8 GiB = 64 GiB — a
  1024x reduction from the pre-fix 64 TiB. Regression guard:
  `graph_cache::tests::prod_default_bound_opens_max_cached_graphs_simultaneously`
  opens `MAX_CACHED_GRAPHS` real `GraphStore`s under the production default
  bound, keeps every handle simultaneously live, and exercises the cache's
  fingerprint/eviction path at that exact byte count.

- **Isolation-site audit (issue #25 follow-up): 46 test fixture/output
  directories derived their path from `std::env::temp_dir().join(format!(
  "{prefix}_{}", std::process::id()))`, an issue-#21-class defect the
  original #21/#24 sweep missed.** Found via a soak run that hit
  `tests/multilang_integration.rs`'s `test_multilang_auto_index` and
  `test_language_filter_rust_only` both failing with "Found duplicated
  primary key value sample.rs / sample.py". Root cause: `std::process::id()`
  is identical for every `#[test]` in one binary (all run as threads of one
  process, so it disambiguates nothing beyond a differing literal prefix)
  and, more importantly, can repeat across separate process invocations
  under OS PID reuse — a real risk under a tight back-to-back soak, where a
  leftover DB from a prior run's process can collide with a new run that
  gets the same recycled PID. Fix: every site now derives its directory via
  `tempfile::Builder::new().prefix(tag).tempdir().expect(..).keep()` — a
  cryptographically-random suffix that depends on neither the thread nor
  the OS PID; `.keep()` hands the already-created directory to each test's
  existing manual cleanup instead of auto-deleting it on drop. Full audit
  table (file:line, prior derivation, verdict) is in the issue #25 PR body.

- **`prepare_prd_input` now uses the hybrid BM25/vector search index when one
  exists, instead of always running the substring-only fallback scorer
  (issue #18).** `search_and_classify` (`src/prd_input/matching.rs`)
  unconditionally passed `index_dir: None` to `search::search_graph`
  regardless of whether `analyze_codebase` had already built a
  `search_index/` next to the graph — Stage 4 recall was capped at the
  weakest matcher unconditionally, even when Stage 3d's `search_codebase`
  on the same graph used the real hybrid index. Fix: extracted
  `search::resolve_search_index_dir` (the sibling-`search_index/`-directory
  logic previously inlined in `do_search_codebase`, `src/main.rs`) as the
  single source of truth for resolving a graph's index directory, and
  `prepare_prd_input` now calls it and threads the result through
  `search_and_classify` → `search_hits`. When no index exists, the fallback
  to substring search is now explicit and always logged
  (`eprintln!("[ap] prepare_prd_input: no search_index found ...")`) —
  never silent. Measured on a fixed fixture (`src/prd_input/matching_tests.rs::
  test_issue18_hybrid_index_reduces_spurious_candidates`): substring
  fallback (pre-fix behavior) surfaced 2 spurious `candidate_symbols` from
  unrelated filler words in the description; the hybrid index (post-fix)
  surfaced 1 on the identical graph and description — source: measured on
  2026-07-15, that test's fixture. The 2→1 count is specific to that small
  fixture, not a guaranteed reduction ratio for arbitrary descriptions/graphs
  — the regression test itself asserts `before >= after`, not a fixed delta.

### Changed — `prepare_prd_input` tool schema, `preparer_version` 1.1.0 → 1.2.0

Additive only. `prd_context` gains a new `search_backend` field —
`"hybrid"` when the search index was found and used, `"substring_fallback"`
when none was found — so consumers can see which scorer produced
`matched_symbols`/`candidate_symbols` for a given run.

- **`prepare_prd_input` (feature mode) no longer presents lexical substring
  matches as verified grounding (issue #14).** The matcher ran every
  natural-language word from the description through the graph search with
  `min_score: 0.0` and folded every hit into `matched_symbols` next to a
  bundle-level `verified: true`, so an accidental substring collision (e.g.
  the word "anchor" hitting `_CONCRETE_ANCHOR`) was indistinguishable from a
  real identifier reference. Measured proof: a genuine partial-word hit and
  a false-positive substring hit score IDENTICALLY under the existing scoring
  formula at equal substring-to-name ratio, so no score threshold can tell
  them apart — the fix classifies every hit's `match_mode` (verbatim exact
  citation / exact name match / lexical-only) instead.

### Changed — `prepare_prd_input` tool schema, `preparer_version` 1.0.0 → 1.1.0

**Consumed by `prd-spec-generator` — read this before bumping the pinned AP
version.** All changes are additive to the JSON shape; the semantic change
below is the one to check for in integrating code.

- **`matched_symbols` semantics changed: it can now be empty where it
  previously would not have been.** A description with only lexical
  (non-exact) word overlap against the graph now yields `matched_symbols:
  []` rather than a list of unverified guesses — an empty array is the
  correct, expected output when nothing can be verified, not a bug or a
  sign the pipeline failed. Any consumer that treated a non-empty
  `matched_symbols` as a given must handle the empty case.
- **New per-symbol fields** on every `matched_symbols` entry: `match_mode`
  (`"verbatim"` — identifier cited in the description in backticks and
  resolved exactly; `"exact_name"` — a description word equals the symbol's
  name/qualified-name tail exactly) and `confidence` (the raw search score;
  informational only — trust is carried by `match_mode`, not this score).
- **New `candidate_symbols` array** (`prd_context.candidate_symbols`, same
  shape as `matched_symbols` plus `match_mode: "lexical"`): substring/fuzzy
  hits with no exact-identity evidence. Never folded into `matched_symbols`
  or into `impacted_communities`/`impacted_processes`. Exposed for
  visibility only — do not treat as verified.
- **New `candidate_symbol_count`** field on the `prepare_prd_input` tool
  response, alongside the existing `matched_symbol_count`.
- Cite identifiers in backticks in finding/feature descriptions to get
  verbatim-priority grounding — this is now the reliable way to guarantee a
  specific symbol appears in `matched_symbols`.

## [0.5.0] — Cross-repo bridge: link per-repo graphs at query time

First tagged release since v0.2.2; folds in the untagged 0.3.0 and 0.4.0 work
(those bumped Cargo.toml but were never tagged, so no binaries shipped).

### Added

- **Cross-repo bridge (`src/bridge.rs`).** Links separate per-repo property
  graphs at QUERY TIME via a caller-supplied `sibling_graphs` argument — no
  super-graph merge, no re-index. A reference that dangles in repo A (no local
  definition) is resolved against registered sibling graphs on demand.
  - `resolve_definition` (forward): an unresolved local ref → its definition in
    a sibling repo. Surfaces in `get_symbol`'s miss path as repo-tagged
    `foreign_definitions`.
  - `foreign_callers` (reverse): sibling call sites of a local symbol. Homonym-
    safe — a sibling that locally defines the same short name is skipped, so a
    local call is never mis-reported as cross-repo. Surfaces in `get_impact` as
    a `foreign_callers` section kept distinct from local blast radius, flipping
    the epistemic boundary to lower-bound (name-matched, confidence 0.50).
  - `resolve_graph` reports `cross_repo_resolvable` (how many unresolved refs a
    sibling can define) + a sample.
  - `search_codebase` federates the query across siblings into a bounded,
    repo-tagged `foreign_results` section; the primary cursor stays exact.
  - Optional `sibling_graphs` arg added to all five tool schemas; absent → no-op
    (fully backward compatible). `get_processes` accepts it for API symmetry but
    is documented as not-acted-on (intra-graph by construction; cross-repo would
    require the forbidden super-graph).
- **(0.4.0) Cursor pagination on all bounded reads** — truncation becomes
  pacing across `get_processes`, `get_impact`, `search_codebase`.
- **(0.3.0) Bounded-io** — byte-budgeted MCP responses + read-path graph cache.
- **(0.3.0–0.4.0) Multi-language resolver** — `LanguageProvider` trait lights up
  7 dormant grammars (C/C++/Go/Java/Kotlin/ObjC/Swift); process-grouped search
  via an additive `by_process` index.

## [0.2.2] — Remove the search-index env-var channel (flaky-test root cause)

### Fixed

- **Root-caused the `stage3d_hybrid_search` flake.** v0.2.1 serialized the
  tests with a mutex — a band-aid. The structural cause was that
  `do_search_codebase` passed the search-index directory to
  `search::search_graph` through the PROCESS-GLOBAL env var
  `AA_SEARCH_INDEX_DIR`, a hidden channel that races across any parallel
  callers (and was wiped+rebuilt mid-read → tantivy `FileDoesNotExist`).
  `search_graph` now takes `index_dir: Option<&Path>` as an explicit
  parameter; the env var and `find_search_index_dir` are deleted. The test
  mutex is removed — the four tests run fully parallel, each passing its own
  index dir (verified 3× green). source: dijkstra root-cause audit.

## [0.2.1] — Release hygiene + flaky-test fix

### Fixed

- **CI flake in `stage3d_hybrid_search`.** The four hybrid-search tests share
  the process-global `AA_SEARCH_INDEX_DIR` env var; cargo runs them on parallel
  threads, so they stomped each other's index path and `build_search_index`
  wiped a dir mid-read, producing a tantivy `FileDoesNotExist` on the BM25
  store (CI run 26824494088). Serialized the four tests with a shared mutex
  held for each test's duration — deterministic, no new dependency.
- **Version consistency.** `Cargo.toml`, `.claude-plugin/plugin.json`, and
  both `.claude-plugin/marketplace.json` fields are now all `0.2.1` (the 0.2.0
  release shipped with `plugin.json`/`marketplace.json` lagging). `SERVER_VERSION`
  derives from `CARGO_PKG_VERSION`, so the MCP handshake follows automatically.

## [0.2.0] — All-file indexing

### Added

- **The indexer now indexes ANY file type, not just the tree-sitter language
  set.** Previously `collect_source_files` dropped every file whose extension
  had no parser (`.js`, `.md`, `.json`, `.css`, `.html`, `.txt`, `.pdf`,
  `.docx`, …), so a session touching those files had nothing to navigate to.
  Now, when no language filter is given, the walker collects every file and
  each becomes a `File` node (path / name / extension / size) — binary
  documents included (metadata only; content is never read for them, so
  `.pdf`/`.docx` are safe). Build/dependency dirs are still pruned and a
  language-scoped re-index (`language_filter = Some(L)`) is unchanged.
- **Light cross-file linking for non-AST files** (`src/indexer/light_link.rs`),
  run as a forward-reference-safe post-pass once every `File` node exists:
  - JavaScript family (`.js/.jsx/.mjs/.cjs`): relative `import … from "X"`,
    `require("X")`, dynamic `import("X")` → `Imports_File_File` (Node-style
    suffix resolution).
  - Markdown (`.md/.markdown/.mdx`): inline links `[text](path)` → new
    `References_File_File` edge (doc→file reference), resolved relative to the
    doc and repo-root. External URLs / anchors / absolute paths are dropped.

### Schema

- New `References_File_File` rel table (resolution rel: `confidence`,
  `resolution_method`).

### Tests

- `test_all_file_indexing_documents_and_links`: indexes code + JS + Markdown +
  JSON + txt + binary `.pdf`/`.docx`; asserts all 9 become `File` nodes and
  that Markdown References + JS Imports resolve.

### Fixed

- **Java `implements` and `extends` produced no graph edges.** The Java parser
  emitted them only as `ExtractedRef`s, which the indexer drops, and never
  populated the `bases` / `implements` node columns the resolver reads — so
  `resolve_extends` / `resolve_implements` had nothing to work from. The parser
  now writes both columns (mirroring `parser/rust.rs`). Additionally, the
  interface-name extraction iterated the `super_interfaces` node's direct
  children and so never found the type identifiers (they sit one level down in
  a `type_list`); `extract_interfaces` now descends into the `type_list`. Java
  `class Dog extends Animal implements Greeter` now yields `Extends_Struct_Struct`
  and `Implements_Struct_Trait` edges.

## [0.1.0] — History layer, declared-implements resolution, indexer batching, all-direction get_impact

> **No `v0.1.0` tag exists on this repository.** Audited 2026-08-10 against
> every published tag: this section was never cut as a release (see
> `v0.0.9` immediately below, which IS tagged, and `v0.2.0`, the next tag
> up). Left in place, undated, per changelog provenance discipline — no
> historical entry is rewritten or deleted. No breaking change is buried
> here: the one behavior change below (`get_impact`'s response gained
> fields) is additive, not a removal or a shape change to an existing
> field.

### Added

- **Code-history temporal layer.** New `Commit` and `Version` node tables plus
  `PreviousVersion` (commit ancestry + per-entity version chain), `ChangedIn`
  (version→commit) and `VersionOf` (version→File/Function/Method/Struct/Enum/
  Trait) relationship tables. A new `index_history` MCP tool walks `git log`,
  persists commit metadata + ancestry, then records a `Version` per (entity,
  commit) for every File and symbol a commit changed. The graph is now
  traversable across time in both directions:
  `entity ← VersionOf ← Version → ChangedIn → Commit → PreviousVersion → Commit`.
  File attribution is exact; symbol attribution maps changed lines onto the
  current graph's symbol ranges. Implemented in `src/history/`.
- **Declared `Implements` resolution.** New `implements` column on Struct/Enum
  (derived/declared trait names) and `trait_name` column on Method (the trait
  of an `impl Trait for Type` block — already extracted by the parser but
  previously dropped for lack of a column). `resolve_implements` now resolves
  these **declared facts** — to a local `Trait` (`Implements_*_Trait`) or, for
  `#[derive(...)]`, to a stdlib trait via the macro-expansion table
  (`Implements_*_StdlibSymbol`, e.g. `Debug → std::fmt::Debug`) — wiring the
  previously-unread `macro_expansion::emit_implements`.

### Changed

- **`get_impact` returns the real blast radius.** Previously it returned only
  community + process membership. It now also returns reverse dependencies —
  `callers`, `importers`, `users`, `implementors` — each as a re-queryable
  `{id, qualified_name, label}` handle so a consumer (Cortex, an agent) keeps
  traversing through MCP instead of receiving a terminal digest.
- **Indexer batches inserts across files.** Symbol nodes/edges now accumulate
  into a `SymbolBatch` and flush in large batches instead of one small bulk
  call per file. Indexing the 500-file synthetic fixture dropped from ~140 s to
  ~8 s (~17×); the `scalability_bench` 60 s budget now passes with wide margin.
- **`clustering.rs` (1061 lines) and `indexer.rs` (832 lines)** split into
  `src/clustering/{community,process,impact}` and `src/indexer/{walk,persist}`
  directory modules to satisfy the 500-line-per-file limit. Behaviour-preserving.

### Fixed

- **Process call-chains were flattened.** `ParticipatesIn` edges hardcoded
  `depth = 0`, discarding the BFS distance that was already computed. They now
  carry the real per-step depth, so a process's participants can be ordered.
- **`#[derive(...)]`, `impl Trait for`, and Java `implements` produced no (or
  wrong) `Implements` edges.** The indexer dropped the parser's implements refs
  and the resolver fell back to a fuzzy method-name-match heuristic (false
  positives + missing every declared impl). Replaced by declared resolution
  (see Added).

## [0.0.9] — Skip build / dependency dirs at walk time (Android, iOS, Go, JVM)

### Fixed

- **Indexer wasted minutes walking into `build/`, `Pods/`, `DerivedData/`,
  `.gradle/`, `vendor/` etc.** on multi-language repos. The previous
  `should_skip` only filtered Rust / JS / Python conventions
  (`target`, `node_modules`, `__pycache__`, `.venv`, hidden dirs), so
  Android codebases (`app/build/intermediates/`, `feature/*/build/`)
  produced tens of thousands of file stat() calls and per-file size
  rejections after the walker had already descended into them. On a
  large Android tree this manifested as `ingest_codebase` appearing
  to hang. Filtering at the directory level avoids the descent
  entirely.
- Extended `should_skip` to cover: `build`, `out`, `.gradle`, `.idea`
  (JVM / Android), `Pods`, `DerivedData`, `.build`, `Carthage`,
  `.swiftpm` (Apple), `vendor` (Go), `dist`, `bin`, `obj`, `coverage`,
  `.nyc_output`, `.pytest_cache`, `.mypy_cache`, `.tox`, `.eggs`.

## [0.0.8] — Multi-language parser expansion (Java, Kotlin, Swift, Objective-C, C, C++, Go)

### Added

- **Seven new tree-sitter parsers** under `src/parser/`:
  `java.rs`, `kotlin.rs`, `swift.rs`, `objc.rs`, `c.rs`, `cpp.rs`, `go.rs`.
  Adds JVM (Java + Kotlin), Apple (Swift + Objective-C), systems
  (C + C++) and Go to the previously-shipped Rust / Python / TypeScript
  trio. `parser/mod.rs` registers all 10 languages; `tool_schemas.rs`
  exposes them in `index_codebase` / `analyze_codebase` language hints.
- **Grammar dependencies** (Cargo.toml): `tree-sitter-java`,
  `tree-sitter-kotlin-ng`, `tree-sitter-swift`, `tree-sitter-objc`,
  `tree-sitter-c`, `tree-sitter-cpp`, `tree-sitter-go`. All MIT or
  Apache-2.0; all official tree-sitter grammars on crates.io.

### Changed

- `do_analyze_codebase`: replaced the explicit Rust/Python/TypeScript
  match with a generic `lang.as_str()` dispatch so LSP-enhanced
  resolution flows through to every supported language.
- Each new parser extracts typed symbols matching the existing
  `graph_store` schema (entities + edges) so the property graph
  remains polyglot-uniform.

### Migration notes

- First build is slower: each new tree-sitter grammar carries C
  source that must compile through `cmake` / `cc`. Subsequent
  incremental builds reuse the per-grammar caches.

## [0.0.7] — Rename binary `ai-architect-mcp` → `automatised-pipeline`

### Changed

- **Binary renamed** from `ai-architect-mcp` to `automatised-pipeline`
  to match the project / plugin / repository name. The Cortex
  `ap_bridge.py` allowlist already accepts `automatised-pipeline`;
  the legacy `ai-architect-mcp` identifier was a stale carryover from
  the project's earlier life as the umbrella `ai-architect` pipeline.
  Affected files: `Cargo.toml` `[[bin]] name`, `bin/ensure-binary.sh`,
  `.mcp.json`, `.github/workflows/release.yml`, `.claude/hooks/session-start.sh`.

### Migration notes

- Release artifacts are now named `automatised-pipeline-{os}-{arch}.tar.gz`
  (was `ai-architect-mcp-*`). Consumers (e.g. Cortex `pipeline_install_release.py`)
  must update their download URLs.
- Built binary path is now `target/release/automatised-pipeline`.
- The Rust crate name (`[package].name`) is unchanged at `ai-architect-mcp`
  to preserve crate identity for any downstream Cargo dependents.

## [0.0.6] — Self-locating plugin MCP launcher

### Fixed

- **`ai-architect` MCP server failed to connect from any non-plugin CWD.**
  The `.mcp.json` launcher relied on Claude Code injecting
  `CLAUDE_PLUGIN_ROOT`, which was not happening reliably. The fallback
  `${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "$0")" && pwd)}` is broken
  under `bash -c` (where `$0` is `bash`, not the script path), so
  `$ROOT` resolved to the user's project directory — where
  `target/release/ai-architect-mcp` does not exist. Replaced the bash
  command with a Python one-liner that reads
  `~/.claude/plugins/installed_plugins.json` (always at a fixed
  absolute path) to discover the plugin install path, then `execvp`s
  the Rust binary, falling back to `bin/ensure-binary.sh` if the
  binary is missing. No CWD or env dependency. Users in any project
  now get the MCP server on plugin update — no per-project
  configuration required.

## [0.0.5] — Resilient install: pre-build the MCP binary

### Fixed

- **Inline `cargo run --release` fallback in `.mcp.json` blocked MCP
  startup.** When `target/release/ai-architect-mcp` was absent (fresh
  install or first session after a checkout), the launcher invoked
  `cargo run --release`, which can take 2–3 minutes for a cold rust
  toolchain. Claude Code's MCP startup timeout fires long before that,
  so the server appeared "disconnected" with no actionable message.
  Replaced with a fail-fast launcher: check binary → if missing, run
  `bin/ensure-binary.sh verbose` → re-check → if still missing, exit
  1 with a `FATAL` message printing the exact `cargo build` command
  to run. Never compiles inline during MCP startup.

### Added

- `bin/ensure-binary.sh` — idempotent build script. Exits 0 fast when
  `target/release/ai-architect-mcp` exists and is newer than every
  file under `src/` and `Cargo.{toml,lock}`. Otherwise runs
  `cargo build --release` with progress on stderr only (stdout is
  reserved for the MCP protocol). Distinct exit codes:
  127 (cargo not in PATH), 1 (build failure or post-build sanity
  failure). Runs in two modes: `quiet` (default; errors only) and
  `verbose` (progress + timing).
- `session-start.sh` hook now invokes `ensure-binary.sh verbose`
  BEFORE Claude Code attempts to connect MCP servers. First-time
  install builds the binary synchronously during the session-start
  banner; subsequent sessions exit instantly. Hook continues even on
  build failure — the `.mcp.json` launcher surfaces the error
  cleanly on `/mcp`.

## [0.0.4] — Idempotent BM25 index rebuild

### Fixed

- `search::bm25::build_index` now wipes ``index_dir`` before calling
  `Index::create_in_dir`. Tantivy refuses to reuse a directory that
  already contains an index (`Index already exists`), so consecutive
  runs of `analyze_codebase` (e.g., Cortex's `ingest_codebase` with
  `force_reindex=true`) failed with that error. The BM25 index is a
  derived artifact rebuilt from the live graph, so removing it is
  safe.

## [0.0.3] — Schema-guarded edge resolution

### Added

- `is_known_rel_table` helper in `graph_store.rs` — public predicate
  over `REL_TABLES` so producers that build relationship-table names
  from runtime symbol labels can validate before insertion instead of
  failing inside the graph driver.
- `Imports_File_Method` declared in `REL_TABLES`; previously a method
  imported directly from a file produced a dropped edge with no
  recoverable target table.

### Fixed

- `resolver::resolve_single_import`, `resolve_glob_import`,
  `resolve_calls`, and `resolve_field_type_uses` now consult
  `is_known_rel_table` before staging an edge. Unknown labels are
  logged (first 8 occurrences via an `AtomicU64` counter to bound
  log volume) and the edge is dropped — this replaces the previous
  hard failure path when a new caller/target label combination
  appeared at runtime.
- `lsp_resolver::try_add_lsp_edge` applies the same guard to
  LSP-derived edges (rust-analyzer / pyright / tsserver).

### Added — public-readiness baseline (carried over from Unreleased)

- Public-readiness baseline: LICENSE (MIT, sole independent author),
  CONTRIBUTING.md, CODE_OF_CONDUCT.md, SECURITY.md.
- GitHub issue templates (bug / feature / audit-finding) and PR template
  with audit-cycle checklist.

## [0.0.2] — Stage 1–9 wired + 23 MCP tools

### Added

- 23 MCP tools across pipeline stages 0 through 9:
  - Stage 0: `health_check`
  - Stage 1: `extract_finding`, `refine_finding`
  - Stage 2: `start_verification`, `append_clarification`,
    `finalize_verification`, `abort_verification`
  - Stage 3a: `index_codebase`, `query_graph`, `get_symbol`
  - Stage 3b: `resolve_graph`, `lsp_resolve`
  - Stage 3c: `cluster_graph`, `get_processes`, `get_impact`
  - Stage 3d: `search_codebase`, `get_context`, `analyze_codebase`,
    `detect_changes`
  - Stage 4: `prepare_prd_input`
  - Stage 6: `validate_prd_against_graph`
  - Stage 8: `check_security_gates`
  - Stage 9: `verify_semantic_diff`
- LadybugDB property graph with 16 node labels, 36+ relationship tables.
- tree-sitter AST extractors for Rust, Python, TypeScript.
- Cross-file resolution (imports, calls, impls) with confidence scoring;
  optional LSP deep resolution (rust-analyzer / pyright /
  typescript-language-server).
- Inline Louvain community detection with C2 repair.
- BFS execution-flow tracing from entry points.
- Hybrid BM25 + sparse TF-IDF + RRF search index (Tantivy-backed).
- Tarjan SCC for cycle detection in semantic-diff.
- 220 tests passing, zero clippy warnings, every numeric constant sourced.

### Architecture

- Hand-rolled stdio JSON-RPC 2.0 (no SDK — owns the wire).
- Clean Architecture with strict module boundaries:
  `transport → server → handlers → core modules → persistence`.

---

For pre-0.0.2 history (initial scaffolding, dependency selection),
see git log. The project entered semantic-versioned releases at v0.0.2.
