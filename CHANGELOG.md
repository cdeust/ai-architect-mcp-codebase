# Changelog

All notable changes to this project will be documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

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
  io.github.cdeust/automatised-pipeline`) is now visible README text in
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
