# ADR-0056: Adopt CBM's Proven Shallow-Spec Model for Language Breadth

**Status:** Accepted — maintainer directive 2026-07-26.
**Date:** 2026-07-26
**Decision-makers:** cdeust
**Supersedes:** nothing. **Completes** ADR-0055 §2's unbuilt **Tier-2** tier, which is the maintainer's recorded acceptance condition on ADR-0055 ("languages unbounded via Tier-2 rows").
**Related:** issue #60 (closes its Done criterion); ADR-0055; reference implementation `DeusData/codebase-memory-mcp` @ `97ce23f` (v0.9.0), read from source for this ADR; gates `tests/graph_accuracy.rs`, `tests/parser_fidelity.rs`, the five `*_parity_tests.rs`. Standards §1.2 (OCP), §2.2 (layer rule), §7.2, §8 (sources), §9 (no speculative fields), §13, §15.

> **Revision note.** An earlier draft of this ADR proposed (a) replacing the `LanguageConventions` trait with per-language *strategy enums* and (b) storing specs in a database. Both were withdrawn. (a) was correctly identified by the maintainer as re-creating the trait under another name — it trips the very Risk #1 that draft recorded. (b) rested on the belief that CBM stores languages in a DB; **reading CBM's source falsified it** (evidence below). This revision is written against measured facts rather than the ADR-0055 summary of CBM.

---

## Context

### What issue #60 actually requires, still unmet

#60's Done criterion: *"one NEW language added **purely via spec entry**"*. Five languages have been migrated onto `src/parser/spec/` at exact parity (ADR-0055 phases 1–5), yet the eleventh language still costs Rust: `LanguageConventions` (`src/parser/spec/conventions.rs:110-271`) declares **six required methods** (`visibility_of`, `receiver_type`, `def_qn`, `call_callee`, `call_entry`, `imports_of`). A language that does not implement all six does not compile.

Meanwhile AP reads 10 languages and CBM answers on 163. The gap is the strategic problem; §15 says the criterion is the contract.

### What CBM actually does — measured, not assumed

Read from `DeusData/codebase-memory-mcp` @ `97ce23f`:

**Its spec table is pure node-type lists.** `CBMLangSpec` (`internal/cbm/lang_specs.h:20-42`) carries only NULL-terminated `const char **` arrays per concern (`function_node_types`, `class_node_types`, `field_node_types`, `module_node_types`, `call_node_types`, `import_node_types`, `import_from_types`, `branching_node_types`, `variable_node_types`, `assignment_node_types`, `throw_node_types`, `decorator_node_types`, `env_access_*`), one `throws_clause_field`, a `ts_factory` function pointer, and an `embedded_imports` list. **There is no visibility rule, no QN template, no callee transform, no import-statement dispatch, and no inheritance model in the row.** A row is ~6 lines (`lang_specs.c:1637+`), and families share arrays outright — TSX reuses every TypeScript/JavaScript array.

Their own comment shows they actively resist per-language fields: the one behavioural item they needed (string-dispatch suffixes) was kept **out** of the struct "to avoid `-Wmissing-field-initializers` across 155 language rows" (`lang_specs.h:44-48`).

**There is no language database.** CBM's SQLite tables are `config`, `edges`, `file_hashes`, `nodes`, `projects`, `store_meta`, `index_coverage`, `index_coverage_meta`, `project_summaries`, `guard_wal_sentinel`. No `languages` table exists. `cbm_lang_spec()` is a bounds-checked index into a compile-time `static const CBMLangSpec lang_specs[CBM_LANG_COUNT]` (`lang_specs.c:2644-2649`), and no runtime load of language data from JSON/TOML/SQLite exists anywhere in `internal/cbm/`. CBM's database is its graph store — the role AP's lbug already fills.

**CBM does hardcode per-language cases — more than AP does.** Measured across `extract_defs.c`, `extract_calls.c`, `extract_imports.c`:

| | CBM @ `97ce23f` | AP today |
|---|---|---|
| languages in enum | 163 | 10 |
| **languages with hardcoded conditionals** | **126 (77%)** | 5 (trait impls) |
| per-language conditional sites (`lang == CBM_LANG_*`) | **176** | — |
| extractor LOC | **~12,130** | ~970 behaviour + shared generic walkers |

They look like this (`extract_defs.c:312-334`):
```c
if (lang == CBM_LANG_LUA    && strcmp(kind, "function_definition") == 0) { … }
if (lang == CBM_LANG_OCAML  && strcmp(kind, "value_definition")    == 0) { … }
if (lang == CBM_LANG_ZIG    && strcmp(kind, "test_declaration")    == 0) { … }
```
and they compute visibility language-dependently too (`def.is_exported = cbm_is_exported(name, ctx->language)`, `extract_defs.c:3248`).

### The actual lesson

CBM's breadth does **not** come from a database, and **not** from having eliminated per-language code. It comes from one property AP lacks:

> **A language with only node-type lists still produces useful output.** CBM's generic path yields definitions, calls and imports from node-kind lists alone. Depth is layered on afterwards for the languages that earn it; the long tail keeps the shallow result.

AP inverted this. Every AP language must be *deep or absent*, because the generic walkers depend on six required behavioural methods to emit anything at all. That is what caps AP at 10 — not the absence of a DB.

`if (lang == …)` chains are also the construct §1.2 forbids and ADR-0055 was written to avoid. **AP should adopt CBM's spec shape and its shallow-first tiering — not its conditional chains.**

---

## Decision

**1. Add a shallow, behaviour-free spec: `ShallowSpec`.** Pure node-kind lists plus a grammar factory. No trait, no required methods, no per-language Rust:

```rust
pub(crate) struct ShallowSpec {
    pub language: Language,
    pub function_node_kinds: &'static [&'static str],
    pub class_node_kinds:    &'static [&'static str],
    pub method_node_kinds:   &'static [&'static str],
    pub call_node_kinds:     &'static [&'static str],
    pub import_node_kinds:   &'static [&'static str],
    pub name_field:          &'static str,          // usually "name"
    pub body_field:          Option<&'static str>,
    pub ts_language:         fn() -> TsLanguage,
}
```

**2. One generic shallow walker interprets it, with zero language conditionals.** Its three grammar-agnostic rules — no per-language keyword lists, no visibility heuristics:

- **Definitions.** A `function_node_kinds` node ⇒ `Function`, or `Method` + `HasMethod` when walked inside a `class_node_kinds` body. A `class_node_kinds` node ⇒ `Struct`, recursed into. Name = the `name_field` child; QN = `scope::name`, deduplicated by `@line` on collision.
- **Calls.** A `call_node_kinds` node ⇒ `CallSite` + `Calls`, callee = its first **named** child's text reduced to the last qualified segment.
- **Imports.** An `import_node_kinds` node ⇒ `Import` + `Imports`, path = its **named children's** text. This is the key generic trick: in every tree-sitter grammar the keywords (`import`, `from`, `;`) are *unnamed* children, so reading only named children strips them **without a per-language keyword list**.

**3. Output is deliberately shallow, and its gaps are explicit rather than guessed.** A shallow language emits `Function`/`Method`/`Struct`/`CallSite`/`Import` nodes and `Defines`/`HasMethod`/`Calls`/`Imports` edges. It emits **no** visibility, **no** inheritance edges, **no** import aliasing, and no language-specific QN scheme.

Visibility is left **empty, never inferred**. A name-case guess (uppercase ⇒ public) is correct for Go and wrong for Java, Swift, Kotlin and C#, which carry visibility in modifier keywords — it would write a plausible-but-false property into the graph, which §13.1 F2 forbids as a silent degraded mode. An absent field is honest; a wrong field is a defect.

**4. Shallow status is a first-class, queryable signal (§13.1 F1/F2).** Each shallow language is marked in the spec registry, and its extraction depth is reported through the existing coverage-honesty surface (issue #57's mechanism), so a consumer can tell "this repo has no `Extends` edges because Ruby is shallow" from "this repo genuinely has no inheritance". A degraded mode that cannot be observed is the FlashRank failure this repo already paid for.

**5. The five existing languages keep their depth. No regression.** Go/Python/Java/Kotlin/Swift stay on the deep path, with their exact-parity suites, per-EdgeKind F1 = 1.000 and `graph_accuracy` 41/41 unchanged. Deleting their visibility/inheritance/import-alias extraction would regress those gates and the resolver that reads those properties, and would make AP *worse* than the reference rather than broader. Depth is AP's differentiator; breadth is what it lacks.

**6. No database for specs.** The reference implementation has none; a runtime spec store would add schema, migration, caching and a startup-failure path while trading compile-time checking for runtime validation, and `ts_language` is a linked fn pointer so a DB row still could not introduce an unlinked grammar. Specs stay a compile-time Rust table — exactly CBM's arrangement. **Should specs later need to be user-editable, the honest first step is a JSON/TOML file loader behind a port, not a table in the per-project graph DB** (which would create a bootstrap cycle: the parser *produces* that graph).

### The grammar boundary, stated plainly

`ts_language` is a function pointer into a linked grammar crate. **No spec mechanism can introduce a grammar that is not compiled in** — this is what linking means, and it is equally true of CBM (`ts_factory` resolves at link time, `lang_specs.h:56-58`).

So adding a language is: **one `Cargo.toml` dependency + one registry row + one `ShallowSpec` literal + one fixture. Zero walker code, zero conventions code, zero conditionals.** #60's *"purely via spec entry"* is met in the sense that matters, and the residual Cargo dependency is irreducible.

---

## Rules compliance

| Rule | Effect | Verdict |
|---|---|---|
| §1.2 OCP | Adding a language becomes a data row with **zero** edits to walker code. Explicitly rejects CBM's 176 `if (lang == …)` sites, which are the growing-conditional-chain §1.2 forbids. | **Pass — and avoids the reference's violation** |
| §1.4 ISP | A shallow language implements **nothing**. Contrast the current six required methods, two of which Swift satisfies with documented-unreachable stubs (`swift.rs:128-144`). | **Pass — strong** |
| §2.2 Layer rule | `src/parser/` stays I/O-free (today it imports only `std::time` + `tree_sitter`). No DB dependency is introduced — a direct consequence of decision 6. | **Pass** |
| §3.3 Reusability | `ShallowSpec` is added because a *second* extraction depth is now a real, demanded use — not speculative generality. Deep and shallow are two concrete uses. | **Pass** |
| §7.2 | No new global mutable state; the registry stays a `static` table. No dynamic dispatch is added — the shallow path has no trait object at all. | **Pass** |
| §8 Sources | Every node-kind string in a shallow row must cite that grammar's `node-types.json`, and the existing `guard.rs` validator must be extended to shallow rows so the citation stays executable. | **Pass — must extend the guard** |
| §9 No dead/speculative fields | Every `ShallowSpec` field is read by the shallow walker; none is reserved. Fields CBM carries that AP's graph has no node for (`branching_node_types`, `throw_node_types`, `env_access_*`) are **deliberately omitted** until a consumer exists. | **Pass** |
| §13 F1/F2 | Shallow extraction is a named, observable degraded mode (decision 4), never a silent default; visibility is absent rather than guessed (decision 3). | **Binding** |
| §12 Mutation | The shallow walker is one code path exercised by every shallow language's fixtures; mutants there are killed by N corpora at once. | **Pass** |

### Risk #1 (successor to ADR-0055's)

**Shallow-path conditional creep.** The failure mode is a `if spec.language == Ruby` appearing in the shallow walker — CBM's exact defect. **Tripwire: the shallow walker must contain zero references to specific `Language` variants, enforced by a test that greps its own source.** A language that cannot be expressed by the shallow fields is either promoted to the deep path or left unsupported (Tier 3) — it does not earn a conditional.

ADR-0055's Risk #1 fired five times before being acted on. This one is machine-checked from the first commit.

---

## Rollout

| Step | Change | Proof |
|---|---|---|
| **1** | `ShallowSpec` + generic shallow walker + registry wiring + `guard.rs` extension + the zero-conditionals tripwire test. | Existing suites untouched (5 parity + `graph_accuracy` 41/41 + `parser_fidelity`); new unit tests for defs/calls/imports on a shallow fixture. |
| **2** | Add the first NEW languages as pure rows (Ruby, then PHP). | Fixture-repo extraction asserted; **closes #60**. |
| **3** | Shallow-depth reporting through the coverage-honesty surface. | Signal-emission asserted by test (§13.1 F1). |
| **4** | Widen the shallow set (Scala, C#, Elixir, Zig, …), one row + one fixture each. | Per-language fixture assertions. |

Steps 1–4 are purely additive: no existing language changes path, so no existing gate can move. The four Swift gaps (#97–#100) are deep-path work and remain sequenced separately.

---

## Consequences

**Positive.** Language count stops being gated on engineering. #60's criterion becomes reachable. AP keeps a genuine advantage over the reference: its five deep languages extract visibility, inheritance and import aliasing that CBM's generic path does not, while breadth stops being a liability. The shallow walker is one code path under N corpora of mutation pressure.

**Negative / accepted.**
- **Two extraction depths coexist.** Justified: it is exactly the tiering ADR-0055 §2 already specified and the maintainer already accepted, and it is how the reference achieves breadth. The cost is one clearly-labelled boundary, not a per-language branch.
- **Shallow languages give thinner answers.** Accepted deliberately, and made *observable* (decision 4) rather than hidden. This is the honest version of CBM's tradeoff, which reports 163 languages without disclosing that 77% carry hand-written conditionals and the tail is shallow.
- **`guard.rs` must cover shallow rows**, or a stale node kind degrades silently — the failure mode ADR-0055 called "the single most important guard".

**Neutral.** Public output contract, graph schema and MCP API unchanged. The five hand-written walkers not yet migrated (Rust, TypeScript, C, C++, Obj-C) are untouched.
