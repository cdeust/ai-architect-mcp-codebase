# ADR-0055: Table-Driven Language Specs — Scale Extraction Beyond the Core Grammars

**Status:** Accepted — maintainer approval 2026-07-25; implementation tracked in #60.
**Date:** 2026-07-25
**Decision-makers:** cdeust
**Related:** issue #60 (arch: table-driven language specs); reference design `cbm/internal/cbm/lang_specs.h` (CBM `CBMLangSpec`); existing house pattern `src/language_provider/mod.rs` (trait+registry) and `src/macro_expansion.rs` (`get_macro_table`); accuracy gate `tests/graph_accuracy.rs`; fidelity gate `tests/parser_fidelity.rs`. Coding standards `~/.claude/rules/coding-standards.md` §1 (SOLID), §2 (Clean Architecture), §5 (reverse DI / factory), §7 (local reasoning), §8 (sources).

> This is an ADR-only artifact filed ahead of any code, per §10 (High-stakes ⇒ ADR-first) and the Done criteria of issue #60 ("ADR accepted" precedes "one NEW language added purely via spec entry"). It records the decision, its blast radius, and the discipline that follows. It is not an implementation and ships no code.

---

## Reversibility classification (architect Move 6)

- **Classification:** **High — Type-1 (one-way door) at the architecture level, executed via Type-2 (reversible) per-language steps.**
- **Criterion that placed it:** "introduces a pattern not already in the codebase" + "affects >5 modules structurally" (the extraction engine underneath every parser) + language coverage is a strategic/competitive commitment external contributors will build on. The *commitment to a spec-table extraction architecture and its spec schema* is hard to reverse once 15–20 language rows and outside contributors depend on it. Each *individual language migration*, however, is guarded by the accuracy gate and revertible in isolation.
- **Discipline applied:** full architect Moves 1–5 + 7 + 8; full §-rule enforcement (High-stakes, no exceptions without an ADR — this is that ADR). Rollout is strangler-fig / parity-gated, one language at a time, so the one-way architectural door is walked through in reversible steps.

---

## Rules compliance audit (`~/.claude/rules/coding-standards.md`)

| Rule | Affected by this decision | Pass / Exception |
|---|---|---|
| §1.1 SRP | Separates "how to walk an AST" (extraction algorithm — one reason to change) from "what node kinds mean in language L" (grammar facts — a different reason to change). Today these are tangled in each `src/parser/<lang>/extract/*.rs`. | **Pass — improves SRP** |
| §1.2 OCP | Adding a language becomes a new data row + one registry entry, zero edits to walker code — a dispatch table, not a growing conditional chain. Textbook OCP. | **Pass — strong** |
| §1.5 DIP / §5 | Generic walker depends on the `LangSpec` abstraction; grammar crates are the injected detail; the registry is the composition root. Mirrors the existing `LanguageProvider` trait+registry (§5.2). No service locator, no global mutable state (§5.3). | **Pass** |
| §2.2 Layer rule | `parser` stays in its layer. The spec table is pure data (core-like, zero I/O); grammar factories are the only detail. The public output contract (`ParseResult`/`ExtractedNode`/`ExtractedRef`) is unchanged, so no consumer layer is touched. | **Pass** |
| §4.1 File size (500) | The generic walker must stay < 500 LOC. The spec registry is *data* (analogous to CBM's 170 KB table) — one row-module per language keeps any single file small; a data table that exceeds 500 lines is marked `// data table` per the §4.1 auto-generated exception spirit. | **Pass with a size-watch note** |
| §7.2 Macros / codegen | Alternative B (a proc-macro that generates per-language walkers) is **rejected** specifically to honor §7.2: a data table consumed by an ordinary, readable generic walker preserves local reasoning; a macro hides the walker body at the spec site. | **Pass — the rule drives the decision** |
| §8 Sources | Every node-kind string traces to that grammar's `node-types.json`. Today those citations live in comments (`TS_* // source: node-types.json`); this decision makes the citation **executable** via a build/test-time validator (see Consequences). | **Pass — strengthens §8** |

No rule requires an exception. The decision moves several rules from "met by discipline" to "met by construction."

---

## Context

### The as-built shape: per-language walkers, measured

AP's extraction engine lives under `src/parser/<lang>/`. Each supported language is a hand-written tree-sitter walker. Concrete measurement (2026-07-25, `wc -l`):

| Language | LOC | Files | Node-kind constants (`const TS_*`) |
|---|---|---|---|
| rust | 1264 | 6 | 26 |
| typescript | 1089 | 6 | 21 |
| python | 781 | 5 | 9 |
| objc | 735 | 5 | 14 |
| cpp | 523 | 4 | 12 |
| swift | 523 | 4 | 11 |
| kotlin | 490 | 4 | 12 |
| c | 465 | 4 | 8 |
| java | 440 | 4 | 13 |
| go | 405 | 4 | 8 |
| **Total** | **~6,715** | **46** | **134** |

The codebase says "7 languages" in several doc comments (`src/parser/mod.rs:2`, `language_provider` header, Cortex ADR-0052); the tree now carries **10**. The doc/code drift is itself a symptom: adding a language is enough of a slog that the prose doesn't keep up.

**The duplication is structural, not incidental.** Across the ten `extract/` trees the *same walker concepts* are re-implemented once per language against different node-type strings (`grep -rho "fn extract_[a-z_]*"`, deduped):

- `extract_function` — **8** near-identical implementations
- `extract_calls` — **7**
- `extract_import` — **6**
- `extract_class` / `extract_class_like` — **7**
- `extract_method`, `extract_enum`, `extract_top_level`, `extract_call_sites`, `extract_field` — 2–4 each

Each implementation differs almost entirely in *which node kinds it matches* — i.e. in data, not algorithm. `src/parser/python/extract/g1.rs:20-38` is a `match child.kind()` over `TS_FUNCTION_DEF | TS_CLASS_DEF | TS_IMPORT_STMT | …`; the Go, Java, Kotlin, Swift equivalents are the same shape over different constants.

### What adding language #11 costs today

To add, say, Ruby right now:

1. New `src/parser/ruby/mod.rs` — grammar wiring + ~9–26 `TS_*` constants (~150–250 LOC).
2. New `src/parser/ruby/extract/g1..g3.rs` — bespoke walkers for functions, classes, methods, imports, calls, fields, constants (~250–780 LOC). **This is the dominant cost** and it structurally duplicates code that already exists 6–8×.
3. `src/parser/language.rs` — enum variant + 3 match arms (`from_extension`, `as_str`, `from_str_opt`).
4. `src/parser/mod.rs` — `pub mod ruby;` + one `parse_file` dispatch arm.
5. `src/language_provider/mod.rs` — a `LanguageProvider` impl + registry arm (resolver conventions).
6. `Cargo.toml` — the grammar crate.
7. `tests/` — fixtures + `graph_accuracy` floors + `parser_fidelity` assertions.

Net: **~400–1,000 LOC of new hand-written walker code per language**, most of it a structural copy. This is the exact "shotgun surgery ⇒ missing abstraction" symptom named in coding-standards §6.2. It caps AP's realistic reach at ~10 and makes each addition an engineering project rather than a data entry.

### The CBM evidence that spec-tables scale

The reference design `cbm/internal/cbm/lang_specs.h` demonstrates the ceiling. CBM supports **158 languages** with almost no per-language code: a single `CBMLangSpec` struct (`lang_specs.h:20-42`) carries NULL-terminated node-type lists per concern — `function_node_types`, `class_node_types`, `call_node_types`, `import_node_types`, `branching_node_types`, `throw_node_types`, `decorator_node_types`, `env_access_*` — plus a grammar-factory pointer (`ts_factory`) and an `embedded_imports` list for host grammars (Vue/Svelte/Astro) that leave embedded JS unparsed (`CBMEmbeddedLangSpec`, `lang_specs.h:13-17`). Generic walkers in `extract_defs/calls/imports` consume the table; the 170 KB artifact is *pure data*. Adding a language in CBM is a table row, not a walker.

That is exactly the inversion AP has not yet made: CBM turned "walker per language" into "one walker + one data row per language." AP already made the *analogous* inversion on the **resolver** side — `src/language_provider/mod.rs:32-100` isolates the six per-language resolution divergences (import separator, prefix-strip, stdlib roots, primitives, macro-table key) behind a `LanguageProvider` trait + registry, so the seven parse-wired-but-resolution-dormant grammars resolve cross-file edges "with no change to the phase logic." The extraction side is the last place the old per-language shape survives.

### Strategic driver

Language coverage is a competitive axis. CBM answers on 158 languages; AP answers on 10. For a codebase-intelligence product the marginal buyer's repo may be Ruby, PHP, Scala, C#, Elixir, Zig — none of which AP reads today. The cost of "yes" must fall from a ~670-LOC engineering task to a data-entry-plus-one-fixture task, or coverage stays a liability. The strategic goal is not "158 grammars in the binary" (see the tiering decision below) but "the *cost* of the eleventh language is data, not engineering."

### Stability / churn context (architect Move 8)

`parser` is a **stable, high-Ca** module: imported by `call_evidence`, `resolver`, `indexer/{mod,persist,incremental,walk}`, `language_provider`, `main`, and 39 call sites; its efferent coupling is tree-sitter + std only (low Ce ⇒ low instability I). That makes it the *right* place to seat the abstraction — but it also means the extraction internals sit under a wide blast radius **unless the public output contract is held fixed** (it is; see Blast radius). Churn over 180 days: **19 commits, 1 author (cdeust)** — moderate, single-author. Implication: the walker logic is well-understood by one person, so an incremental migration where each language's parity is proven in isolation is both feasible and necessary (no second author to cross-check a big-bang rewrite).

---

## Decision (proposed)

Adopt a **table-driven extraction architecture**: a `LangSpec` data structure describing each language's node kinds per concern, consumed by a small set of **generic tree-sitter walkers**, replacing the ten hand-written per-language walkers one at a time behind the accuracy gate. This is the Rust-idiomatic equivalent of CBM's `CBMLangSpec`, and it mirrors AP's own `LanguageProvider` trait+registry that already governs the resolver side.

### 1. The `LangSpec` shape (data, not code)

A Rust struct of `&'static [&'static str]` slices per structural concern — the Rust idiom for CBM's NULL-terminated arrays — plus a grammar factory and an embedded list. Illustrative shape (final field set is an implementation detail for the engineer, not fixed by this ADR):

```
pub struct LangSpec {
    pub language: Language,
    pub function_node_kinds: &'static [&'static str],
    pub class_node_kinds:    &'static [&'static str],
    pub method_node_kinds:   &'static [&'static str],
    pub field_node_kinds:    &'static [&'static str],
    pub call_node_kinds:     &'static [&'static str],
    pub import_node_kinds:   &'static [&'static str],
    pub import_from_kinds:   &'static [&'static str],
    pub constant_node_kinds: &'static [&'static str],
    pub decorator_node_kinds:&'static [&'static str],
    pub extends_field:       Option<&'static str>,   // field name carrying superclasses
    pub name_field:          &'static str,            // usually "name"
    pub body_field:          &'static str,            // usually "body"
    pub ts_language: fn() -> tree_sitter::Language,   // grammar factory (Rust crate)
    pub embedded:    &'static [EmbeddedSpec],         // empty for the core 10
    pub conventions: &'static dyn LanguageConventions,// the behavioral escape hatch — see §4
}
```

Generic walkers (`walk_defs`, `walk_calls`, `walk_imports`) take a `&LangSpec` and produce the **existing, unchanged** `ExtractedNode` / `ExtractedRef` / `ParseResult` types. `parse_file` dispatches through the registry `lang_spec(lang)` instead of a hand-written per-language function.

### 2. Grammar registration and vendoring — a **tiered** model (do NOT vendor 150 blindly)

AP compiles grammars in as Cargo crates (`tree-sitter-python`, `tree-sitter-go`, …). Blind vendoring of 150+ grammars is rejected on binary-size and ABI-drift grounds (the tree-sitter 0.25 / ABI-15 upgrade already forced a coordinated bump for `tree-sitter-swift 0.7.3`, `Cargo.toml:55-58`). Instead:

- **Tier 1 — Core (the current 10):** always compiled in. Full fixture coverage; `graph_accuracy` floor **F1 ≥ 0.92** on Defines/HasMethod/Imports/Calls required; `parser_fidelity` per-language assertions required.
- **Tier 2 — On-demand:** a language added on request, gated behind a Cargo **feature flag** so the default binary stays at the core grammars. Merge requirements per Tier-2 language: (a) an *official / actively-maintained* tree-sitter crate on crates.io, version-pinned in `Cargo.lock`; (b) at least **one fixture repo** with `graph_accuracy` parity at the Tier-1 floor; (c) the spec-validation guard (below) green. A language is data + one fixture — not a walker.
- **Tier 3 — Unsupported:** a spec row may exist without a vendored grammar; `parse_file` returns a clean "unsupported" rather than a silent empty parse.

### 3. Embedded-language re-parse (Vue / Svelte / Astro)

Map CBM's `CBMEmbeddedLangSpec` to an `EmbeddedSpec { script_node_kind, content_node_kind, embedded_language }`. A generic embedded walker locates each `script_node_kind` in the host AST, takes its `content_node_kind` child's byte slice, re-parses it with the embedded language's grammar+spec, and runs the *same generic extractors* on the inner AST. AP handles no embedded languages today (`grep` confirms no `raw_text`/`script_element` re-parse path), so this is **net-new capability the spec model unlocks**, and it is **optional** — `embedded` is empty for all ten core languages. It is explicitly *out of scope for the initial migration* and lands only when a host language (e.g. Vue) is requested as Tier-2.

### 4. The behavioral escape hatch — honest scoping of "data vs code"

AP's extraction is **richer than CBM's** function/call/import triple. It computes visibility (`python_visibility`, `src/parser/python/mod.rs:98`), UPPER_SNAKE constant detection (`is_upper_snake_case`), async flags, decorator CSVs, receiver types, and QN de-duplication for `@property`/`@setter` overloads. Some of these are **behavior, not data** and will not reduce to node-kind slices. Pretending they will is the failure mode that sinks naive spec-table refactors.

Decision: scope `LangSpec` to the **structural** concerns (node-kind → label mapping; def/call/import/field walking; extends field), and keep a thin **`LanguageConventions` trait** — mirroring the existing `LanguageProvider` — for the handful of behavioral predicates (`visibility_of(name) -> String`, `is_constant(name) -> bool`, async/decorator handling). Most languages use a default impl; only the few with real conventions override. This is the Rust-idiomatic, honest split: **data for structure, a small trait for behavior**, both consumed by the generic walkers. It reuses a pattern the codebase already trusts.

### 5. Migration strategy — **incremental strangler-fig, parity-gated. NOT big-bang.**

Argued:

- **The accuracy gate is the invariant.** `graph_accuracy.rs` computes per-EdgeKind F1 with a **≥ 0.92** floor and is designed to move "one at a time" (its own header: "every fix should move the numbers up … the loop continues"). A big-bang rewrite of all ten languages in one PR forfeits this: if F1 drops, you cannot attribute the regression to a specific language's spec. Incremental migration keeps each change independently measurable and independently revertible (**Type-2 per step**).
- **Per-language procedure:** (1) write the language's `LangSpec` row; (2) run the generic walkers against that language's *existing* fixtures; (3) prove F1(Defines), F1(HasMethod), F1(Imports), F1(Calls) each **≥ the language's current measured floor** — parity, not merely ≥ 0.92; (4) only then delete the hand-written `src/parser/<lang>/extract/*.rs`. The old walker stays live until parity holds, so any step reverts cleanly.
- **Order (easiest-to-prove first, most-bespoke last):** start with **Python** (richest `graph_accuracy` fixture coverage — the gate is strongest there) or **Go** (smallest, cleanest grammar), then Java/Kotlin/Swift/C/C++/ObjC, and migrate **Rust last** — it carries the most bespoke logic (1264 LOC) and the tightest coupling to `macro_expansion`, so it is the hardest parity proof and the least safe to move early.
- **The first NEW language (Ruby or PHP, per issue #60 Done criteria) lands only after the core migration proves the model** — as a pure spec row + one fixture repo at Tier-2, demonstrating the cost has actually collapsed to data entry.

Big-bang is rejected: it converts ten independent Type-2 steps into one Type-1 leap with an unattributable failure signal, against a single-author module.

---

## Blast radius (architect Move 4)

**The load-bearing insulation:** the public output contract in `src/parser/mod.rs` — `ParseResult`, `ExtractedNode`, `ExtractedRef`, the `LABEL_*` constants, and the `::`-normalized QN format — **does not change.** Every downstream consumer keys off these, not off the internal walkers. Therefore the blast radius on consumers is **zero by construction, provided parity holds**.

- **Files directly modified (per language migrated):** new `src/parser/spec/` module (LangSpec struct + registry + `walk_defs`/`walk_calls`/`walk_imports` + `LanguageConventions`); the migrated language's `mod.rs` shrinks to a spec row + grammar binding; its `extract/g*.rs` are deleted. `Cargo.toml` unchanged for Tier-1 (grammars are already deps); a `[features]` block added for Tier-2.
- **Transitive callers (unaffected because the contract holds):** `call_evidence`, `resolver`, `language_provider`, `indexer/{mod,persist,incremental,walk}`, `main` — 8 modules, 39 call sites of `parse_file`/`Language`. Verified insulated: they consume `ParseResult`/labels/QNs, not walker internals.
- **Tests affected (the gate — must stay green per language, before any deletion):** `tests/graph_accuracy.rs` (F1 floors), `tests/parser_fidelity.rs` (per-language node-type assertions), `tests/multilang_integration.rs`, `tests/multilang_resolution.rs`, `tests/corpus_full.rs`. **New test required:** a spec-validation test (see Consequences) asserting every node-kind string in every `LangSpec` slice exists in that grammar's `node-types.json`.
- **What the accuracy gate must show, per migrated language, BEFORE the old walker is deleted and BEFORE any new language lands:** F1(Defines), F1(HasMethod), F1(Imports), F1(Calls) each **≥ that language's current measured floor** on its existing fixtures. Parity-first is the non-negotiable invariant. A new language may not land while any core language sits below parity.
- **Graph schema impact:** **none.** Same labels, same edge kinds, same QN format. No migration, no re-index required for existing graphs.
- **MCP API impact:** **additive only.** The `Language` enum may gain variants (Tier-2, feature-gated); existing variants and the tool surface are unchanged. No breaking change to `tool_schemas.rs`.
- **Deploy coupling:** single deployable (the AP binary). No coordinated release. Tier-2 languages are compile-time feature flags, not runtime loads.
- **Recoverability class:** **(a)** for each per-language migration — callers updated (or, here, untouched) within the same PR, no external impact, revertible. **The architecture commitment is (a)-in-execution but Type-1 in intent** — see Reversibility.

---

## Consequences

### Positive
- Adding a language falls from ~400–1,000 LOC of bespoke walker to **one spec row + one `LanguageConventions` default (or small override) + one fixture** — OCP satisfied (§1.2).
- ~6,715 LOC of duplicated walkers collapse toward one generic walker + ten data rows. `extract_function`×8, `extract_calls`×7, `extract_import`×6 become one implementation each.
- Language coverage becomes a *product knob*, not an engineering project — the strategic axis in issue #60.
- Embedded-language extraction (Vue/Svelte) becomes reachable, which the current architecture cannot express at all.
- Node-kind citations move from comment (§8 by discipline) to executable validation (§8 by construction).

### Negative / honest costs
- **Spec tables are data-debugging.** A wrong or stale node-kind string does not error — the walker simply never matches that node, so symbols are **silently dropped**. The failure surfaces only as an F1 dip in `graph_accuracy`, and only if a fixture happens to cover that construct. **Required mitigation (a condition of this decision, not an optional extra):** a **spec-validation guard** that, at build time or in a dedicated test, loads each grammar's `node-types.json` (already the cited source for every `TS_*` constant) and asserts every string in every `LangSpec` slice is a real node kind for that grammar. This converts the silent-drop failure into a loud compile/test failure — the single most important guard in this ADR. Without it, the spec table is *less* safe than the status quo; with it, it is *more* safe.
- **Grammar heterogeneity may not fully reduce to slices.** Some languages identify constructs by field or context, not node kind alone. The `LanguageConventions` escape hatch (§4) absorbs this, but if a grammar needs *structural* (not just behavioral) escape hatches, the generic walker gains conditionals and the DRY win erodes at the margin. Scope discipline (structure = data, behavior = thin trait) is what keeps this bounded.
- **AP's extraction is richer than CBM's**, so AP's spec carries more fields than `CBMLangSpec` and a companion conventions trait — the model is heavier than CBM's pure table. This is accepted as the price of AP's higher-fidelity graph.
- **The spec schema becomes a data contract.** Once Tier-2 contributors author rows, changing a `LangSpec` field touches every row — a mild Type-1 surface on the schema itself. Keep the field set small and stable; changes to it are themselves ADR-worthy.

### Risks — top-3 invalidators in 6 months (architect Move 7, Feynman pass)
1. **Richer-than-CBM extraction leaks back into code.** If enough of AP's per-language behavior (visibility, naming heuristics, decorator semantics, overload dedup) resists the `LangSpec`+`LanguageConventions` split, the "language = data" promise weakens and additions creep back toward engineering. *Watch signal:* a Tier-2 language needing a bespoke walker override. *Guard:* scope the spec to structure; measure the LOC of any per-language override at each addition.
2. **LSP supersession.** `src/lsp_client.rs` / `src/lsp_resolver.rs` exist. If AP pivots extraction toward LSP servers, the tree-sitter spec table is partly superseded. *Watch signal:* resolution accuracy from LSP exceeding tree-sitter extraction. *Guard:* the spec table and LSP are complementary (fast local structure vs. deep cross-file resolution); revisit only if LSP replaces parse-time extraction wholesale.
3. **Coverage may not be the competitive axis assumed.** If ~10 languages already cover ~95% of target repos, the spec-table investment is over-engineering versus hand-writing #11–#13. *Watch signal:* Tier-2 requests staying near zero after launch. *Guard:* the incremental strategy caps sunk cost — the migration pays for itself in de-duplication of the existing 10 even if no eleventh language is ever added, so the downside is bounded.

---

## Alternatives considered

**A. Status quo — one hand-written walker per language.**
Rejected. ~670 LOC bespoke per language; `extract_function` duplicated 8×; caps practical coverage at ~10 against CBM's 158; every addition is shotgun surgery across `parser/language.rs`, `parser/mod.rs`, `language_provider`, `Cargo.toml`, and a new ~670-LOC walker tree (§6.2 "missing abstraction"). Language coverage stays an engineering cost center rather than a data knob. It does, however, remain the fallback for any single grammar that proves genuinely irreducible to a spec (Tier-2 override).

**B. Macro-generated per-language code (a Rust proc-macro emitting walkers from a spec).**
Rejected on two independent grounds. (1) It gives no benefit a data table + generic walker doesn't: both are DRY; the macro merely *generates* the code the generic walker executes at runtime, at the cost of a build-time codegen step. (2) It violates coding-standards §7.2 (macros / codegen are default-refuse — local reasoning defeated): the walker body is invisible at the spec site, and debugging a wrong node-kind means reading generated code. The data-table approach keeps the walker as ordinary, readable, breakpoint-able Rust and the spec as ordinary readable data — the same DRY win with local reasoning intact.

**C. Runtime grammar loading (dylib / `libloading`).**
Rejected. Adds a **trust boundary** (loading arbitrary compiled grammars) and an ABI-drift surface that has *already* bitten this project (the tree-sitter 0.25 / ABI-15 bump, `Cargo.toml:55-58`); tree-sitter grammars cannot be safely loaded across ABI versions without unsafe FFI and version negotiation. It also defeats static dependency analysis, which §5.3 forbids (service-locator-style dynamic wiring). Compile-time grammar crates behind Cargo **feature flags** (Tier-2) deliver on-demand coverage without any runtime loading risk.

**D. Adopt CBM's C `CBMLangSpec` table via FFI.**
Rejected. It pulls a C dependency and a 170 KB C data artifact into a pure-Rust binary, crossing an FFI trust boundary for **zero domain benefit** — the table is *data* AP expresses natively as a Rust struct. CBM resolves `TSLanguage*` at link time from Go tree-sitter modules (`lang_specs.h:54-56`); AP uses Rust grammar crates — the registration stories don't compose. It would couple AP's release cadence to CBM's C build and ABI. **CBM is the reference design, not the dependency:** AP takes the *idea* (node-kind lists per concern + factory + embedded re-parse) and implements it idiomatically in Rust.

---

## Self-verification (architect Move 7)

| Pass | Result | Iteration / hand-off |
|---|---|---|
| Blast radius re-check | Unchanged — public `ParseResult`/`ExtractedNode`/`ExtractedRef` contract held fixed; 8 consumer modules + 39 call sites insulated by construction | none |
| Alternatives audit | All four (status quo, macro-codegen, dylib, CBM-FFI) named with concrete rejection reasons | none |
| Rule compliance | §1/§2/§5/§7/§8 pass; several strengthened (OCP, §8 executable). No exception required | none |
| Reversibility | Type-1 at architecture level, Type-2 per step — consistent before and after blast-radius review; the parity gate is what makes the steps reversible | none |
| Feynman integrity (top-3 invalidators) | Listed in Consequences/Risks: richer-than-CBM leakage, LSP supersession, coverage-not-the-axis | none |
| Churning-module check | `src/parser` = 19 commits / 1 author / 180d — moderate, single-author ⇒ incremental parity-gated migration is the correct discipline (no second author to cross-check a big-bang) | none |

## Hand-offs

- **engineer** — implementation of `src/parser/spec/` (LangSpec, generic walkers, registry, `LanguageConventions`), the spec-validation guard against `node-types.json`, and the per-language migration PRs. This ADR fixes *where the seam is and why*; it writes no production code.
- **Liskov** — before finalizing the `LanguageConventions` trait: confirm every default-impl override preserves the walker's postconditions (no strengthened preconditions, no silently weakened extraction) so language impls are substitutable.
- **Feynman** — recommended review of Risk #1 (richer-than-CBM leakage) on the first two migrations, to confirm the data/behavior split holds empirically rather than by assertion.

## Decision status

**Accepted — maintainer approval 2026-07-25.** Approval condition, recorded verbatim in intent: language coverage must remain unbounded — "if it permits to have as much language as we want" — which the Tier-2 data-row model satisfies; document formats (Confluence wiki, docx) are outside this ADR's scope (they are ingestion adapters, not grammars) and are tracked as separate work items so the condition is not silently dropped. First work item: the `src/parser/spec/` scaffold + the spec-validation guard, then the Python (or Go) migration proven at parity before any walker is deleted.
