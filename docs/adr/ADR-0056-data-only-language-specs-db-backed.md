# ADR-0056: Data-Only Language Specs, DB-Backed — Remove Per-Language Rust From the Extraction Path

**Status:** Accepted — maintainer directive 2026-07-26 ("I don't really [see] the necessity of doing specific classes for C, Swift etc when it will be driven by a db after"); sequencing and DB scope chosen by the maintainer the same day (refactor-first, then the Swift gaps as data; go straight to DB-backed rather than static-tables-first).

> Filed ahead of any code, per §10 (High-stakes ⇒ ADR-first) and issue #60's own instruction ("This is an ADR-grade change — file the ADR before any code"), following the ADR-0055 precedent. This document ships no implementation.
**Date:** 2026-07-26
**Decision-makers:** cdeust
**Supersedes:** ADR-0055 §4 (the `LanguageConventions` behavioral-trait escape hatch) — **partially**. ADR-0055's structural decision (generic walkers over a spec table) stands and is not revisited; its *behavior* half is replaced.
**Related:** issue #60 (the strategic parent — this ADR is what closes its Done criterion); issues #97/#98/#99/#100 (Swift extraction gaps, to be expressed as spec data on top of this); ADR-0055; `src/parser/spec/`; gates `tests/graph_accuracy.rs`, `tests/parser_fidelity.rs`, and the five `*_parity_tests.rs`. Coding standards §1 (SOLID), §2.2 (layer rule), §5 (reverse DI), §7.2 (local reasoning / read-once config), §8 (sources), §10 (stakes), §15 (task definition is the contract).

---

## Context: ADR-0055 did not meet issue #60's acceptance criterion

Issue #60's Done criteria, verbatim: *"ADR accepted; one NEW language (e.g. Ruby or PHP) added **purely via spec entry** with accuracy-gate parity on a fixture repo."*

Five languages have been migrated onto the `src/parser/spec/` seam (Go, Python, Java, Kotlin, Swift — ADR-0055 phases 1–5). The generic walkers work and every migration held exact parity. But **a new language still cannot be added by data alone**, because `LanguageConventions` (`src/parser/spec/conventions.rs:110-271`) declares **six required methods**:

`visibility_of`, `receiver_type`, `def_qn`, `call_callee`, `call_entry`, `imports_of`

A language that does not implement all six does not compile. Adding Ruby today means writing a `RubyConventions` impl — Rust, not a spec entry. #60's criterion is therefore **unmet**, and per §15 the task definition is the contract: the remaining work is not "more migrations", it is removing the required-method wall.

### Measurement: the trait became the bulk, not the escape hatch

ADR-0055 §4 framed the trait as "default impl + per-language overrides only where real conventions exist" — *"data for structure, a small trait for behavior."* Measured on the merged tree (2026-07-26, comment and blank lines excluded, `impl LanguageConventions` block vs the rest of each file):

| Language | code LOC | behavior (`impl LanguageConventions`) | data (`LangSpec` row) | behavior share |
|---|---|---|---|---|
| go | 160 | 143 | 17 | 89% |
| java | 207 | 187 | 20 | 90% |
| kotlin | 297 | 265 | 32 | 89% |
| swift | 221 | ~184 | ~37 | 83% |
| python | 332 | 194 | 138 | 58% |

The "small trait for behavior" is 83–90% of every language file except Python's (whose data row is unusually large). ADR-0055 recorded this as its Risk-#1 watch signal ("richer-than-CBM leakage", measured per migration). **The watch signal has fired for five languages running.** This ADR is the response.

### The behavior is enumerable, not algorithmic

The decisive finding: reading the five impls side by side, the per-language "behavior" is overwhelmingly *the same algorithm parameterised differently* — format templates, node-kind→value maps, and a small closed set of strategies. It is not five different algorithms.

Concrete evidence, taken from the merged source:

**`def_qn` is byte-identical in Go, Java and Swift** (`go.rs:79-81`, `java.rs:131-136`, `swift.rs:146-151`):
```rust
format!("{scope}::{name}#{seq}")
```
Python's differs only by dropping the suffix. This is a **format template**, not behavior.

**`call_entry` is identical in Go, Java and Swift except one string.** All three build the same `CallEntry` — same QN scheme `{caller_qn}::call@{line}:{col}#{seq}`, same `callee_name` property, same `ref_kind: "Calls"`, same line convention. They differ *only* in `visibility`: Go `"public"`, Java `"package"`, Swift `"internal"`. That is **a data field**.

**`call_callee` differs only in where the callee text comes from and whether it is tail-split:**
| Language | text source | transform | accept |
|---|---|---|---|
| go | `function` field | last dotted segment, trim `(` | non-empty ∧ first char alphabetic-or-`_` |
| swift | first named child | last dotted segment, trim `(` | non-empty ∧ first char alphabetic-or-`_` |
| java | `name` field, else `type` field | none | non-empty |
| kotlin | navigation-tail walk | qualifier-or-tail | non-empty |
| python | `function` text | none (full path kept) | non-empty |

Three parameters — *source*, *transform*, *accept* — cover all five.

**`visibility_of` / `node_visibility` is one of four strategies:** name-case (Go: uppercase ⇒ public, else package), name-prefix (Python: `_`/`__dunder__` rules), modifier-keyword (Java from a `modifiers` child; Swift/Kotlin from the declaration head line, with a per-language keyword list and default), or fixed.

**`imports_of` is a table of node-kind→binding-role rows.** Go: DFS to `import_spec_kinds`, read the `path` field, unquote, display = last `/` segment. Java: strip the keywords `import`/`static` and a trailing `;`, display = last `.` segment. Swift: strip the keyword `import`, display = the whole remainder. Python — the richest — dispatches three statement kinds (`import_statement`, `import_from_statement`, `future_import_statement`) over three binding child kinds (`dotted_name`, `aliased_import`, `wildcard_import`). Even Python is a *table*: statement kind × binding kind → (path, alias, glob) roles.

**`receiver_type`** is meaningful only for Go (strip `( )`, take last whitespace token, strip `*`); the other four return an unreachable empty string purely to satisfy the trait.

Of Swift's thirteen overrides, **two are unreachable stubs written only for the trait obligation** (`visibility_of`, `receiver_type`, both annotated as such in `swift.rs:128-144`), two are bare constants (`variant_edge_kind` → `"Defines"`, `variant_visibility` → `"internal"`), and the rest are maps or templates. This is the clearest signal available that the trait is the wrong mechanism: a language is paying a Rust tax to declare *nothing*.

### Why a trait cannot be DB-backed

A DB row can carry a template string, an enum discriminant, or a table of node kinds. It cannot carry `fn call_entry(&self, …) -> CallEntry`. **As long as the six required methods exist, the "driven by a DB" end state is unreachable by construction** — not merely unimplemented. Removing them is the prerequisite, and per §15.1 a missing prerequisite is built, not logged.

---

## Decision

**1. `LangSpec` becomes pure, serialisable data. `LanguageConventions` is deleted.**

Every behavior the trait expressed becomes a declarative field on `LangSpec`, drawn from a small closed set of strategy enums:

```rust
pub struct QnTemplate(String);            // "{scope}::{name}#{seq}"

pub enum VisibilityRule {
    NameCase   { upper: String, other: String },        // Go
    NamePrefix { rules: Vec<(String, String)>, default: String },  // Python
    Modifiers  { source: ModifierSource, keywords: Vec<String>, default: String },
    Fixed(String),
}
pub enum ModifierSource { ChildField(String), HeadLine }   // Java | Swift/Kotlin

pub enum CalleeSource { Field(Vec<String>), FirstNamedChild, NavigationTail }
pub enum CalleeTransform { None, LastDottedSegment }
pub enum CalleeAccept { NonEmpty, IdentifierStart }

pub enum ImportShape {
    PathField  { dfs_to: Vec<String>, field: String, unquote: bool, display: Display },
    StripKeywords { keywords: Vec<String>, strip_suffix: Option<char>, display: Display },
    ChildBindings { statements: Vec<ImportStatementShape> },   // Python
}
pub enum Display { Whole, LastSegment(char) }
```

Plus the already-data-shaped items the trait held as methods: `synthetic_names: Vec<(NodeKind, String)>` (Swift `deinit`/`subscript`), `marker_props: Vec<(NodeKind, String, String)>` (Swift `member_kind`, `typealias`), `label_refinement` (Swift by field value, Kotlin by keyword content), `variant_edge_kind: String`, `variant_visibility: String`, `inheritance: InheritanceModel` (single-list vs split extends/implements), `constant_name_filter: Option<NameFilter>` (Python `UPPER_SNAKE`), `function_props` (Python `is_async`).

The generic walkers *interpret* these fields. One implementation, no per-language dispatch — §1.2 OCP by construction, which is what ADR-0055 claimed and this delivers.

**2. Specs are stored in a tool-global DB and loaded once at startup.**

- A dedicated **spec store**, separate from the per-project graph DB. Rationale below.
- **Seeded from the built-in table on first run**, so out-of-the-box behavior is byte-identical to today and the tool works with no DB provisioning.
- Loaded **once at startup** into a frozen (immutable) in-memory registry. §7.2 permits exactly this ("read-once-at-startup config only"); it is not a mutable global.
- A malformed or unreachable spec store is a **named, signalled degraded mode** (§13.1 F2): the tool logs the failure and falls back to the built-in table rather than silently parsing nothing.

**3. Core stays I/O-free; the DB is an injected adapter (§2.2 / §5).**

`src/parser/` today imports only `std::time` and `tree_sitter` — it is genuinely I/O-free, and the layer rule forbids it depending on a DB. So:

- **Core (`parser/spec`)** defines the `LangSpec` data types and a `SpecSource` port (`fn load(&self) -> Result<Vec<LangSpec>, SpecLoadError>`).
- **Infrastructure** provides `DbSpecSource` (and `JsonSpecSource` for fixtures/tests).
- **The composition root** (startup) calls the source once and installs the frozen registry.
- `parse_with_spec(&LangSpec, …)` keeps its present pure signature. Nothing in the extraction path gains I/O.

### Why NOT the per-project graph DB

The lbug database is opened per project by path (`GraphStore::open_or_create`, called from `src/bridge.rs:186,253,368`) and holds *analysis output*. Putting language specs there would:

- **create a bootstrap cycle** — the parser produces the graph, so it cannot require an open graph DB to know how to parse;
- **duplicate tool-global config into every project DB**, so a spec fix would need re-applying per project;
- **break §2.2**, since `parser` would depend on the graph-store infrastructure.

Language specs are tool-global configuration and get their own store.

### The honest boundary: grammars remain a Rust dependency

`LangSpec.ts_language` is a function pointer into a linked grammar crate (`|| tree_sitter_go::LANGUAGE.into()`). **A DB row cannot introduce a grammar that is not compiled into the binary.** This is not a design shortcut; it is what linking means.

Therefore a **static grammar registry** maps a language name → grammar factory for every vendored crate, and the DB row references it by name. The consequence, stated plainly so no one is misled later:

- Adding a language **whose grammar is already linked** ⇒ **pure data**, no rebuild.
- Adding a language **needing a new grammar** ⇒ one `Cargo.toml` dependency + one registry row, then pure data.

#60's *"purely via spec entry"* is therefore satisfied in the sense that matters — **zero walker and zero conventions code** — and the residual is a vendored dependency, which no architecture can remove. This reading is recorded here so the criterion is judged against a claim that is actually achievable.

---

## Rules compliance audit (`~/.claude/rules/coding-standards.md`)

| Rule | Effect | Verdict |
|---|---|---|
| §1.1 SRP | Sharpens ADR-0055's split: the walker owns "how to walk", the spec row owns "what this grammar means". Removes the third, muddled responsibility — per-language Rust that was partly data, partly algorithm. | **Pass — improves** |
| §1.2 OCP | Delivers what ADR-0055 claimed: a new language is a data row with **zero** edits to walker code. Today it requires a 6-method trait impl. | **Pass — this ADR is the fix** |
| §1.3 LSP | Deletes five trait impls containing methods that exist only to satisfy the trait and are documented unreachable (`swift.rs:128-144`, `java.rs:110-129`) — i.e. current impls weaken postconditions in exactly the way §1.3 forbids. Removing the trait removes the violation. | **Pass — removes a latent violation** |
| §1.4 ISP | The six required methods are a god-interface: Swift implements two of them to return constants it never uses. Data fields are opt-in by absence. | **Pass — improves** |
| §1.5 DIP / §5 | Core declares the `SpecSource` port; infrastructure implements DB/JSON adapters; the composition root wires them at startup. Constructor/parameter injection, no service locator (§5.3). | **Pass** |
| §2.2 Layer rule | `parser` stays I/O-free (it imports only `std::time` + `tree_sitter` today, and still will). The DB adapter lives in infrastructure. Explicitly rejects the per-project-graph-DB option that would have broken this. | **Pass — enforced by design** |
| §4.1 File size | Each per-language file collapses toward a data literal; the interpretation logic is written once in the walkers. Net LOC falls and no new file approaches the cap. Guard: the walkers must stay < 500 (the §101 split already established the module boundaries). | **Pass — monitored** |
| §7.2 Global mutable state | The frozen startup-loaded registry is the *explicitly permitted* "read-once-at-startup config" case, not a mutable singleton. Immutability enforced by type (no interior mutability, no re-load path). | **Pass — permitted case, justified here** |
| §7.2 Dynamic dispatch | **Removes** the `&'static dyn LanguageConventions` dynamic dispatch whose method bodies are per-language and invisible at the call site — the construct §7.2 default-refuses. Data-driven interpretation is locally readable. | **Pass — removes a refused construct** |
| §8 Sources | Every node-kind string keeps its `node-types.json` citation; the existing spec-validation guard (`guard.rs`) must extend to DB-loaded rows, so citations stay executable for data that no longer passes through review. | **Pass — must extend the guard** |
| §12 Mutation | Interpretation logic moves into the walkers, where it is exercised by all five languages' parity suites at once — a mutant there is killed by five corpora instead of one. Strengthens the suite. | **Pass — improves** |
| §13 Definition of Done | Each rollout PR carries a Completion Ledger; the DB path adds explicit rows for load failure, malformed row, missing grammar, and the degraded-mode signal. | **Binding** |
| §15 No deviation | This ADR exists because the spec's own acceptance criterion was unmet. Building the prerequisite rather than logging it is the §15.1 requirement. | **Pass** |

No rule requires an exception. Two rules (§1.3 LSP, §7.2 dynamic dispatch) move from *violated-but-tolerated* to *satisfied by construction*.

---

## Rollout — parity-gated, behavior-preserving, one step at a time

The five existing parity suites are the safety net: they pin each language's **exact** 7-tuple output plus per-EdgeKind F1 = 1.000. Any step that changes them has changed behavior and is wrong.

| Step | Change | Proof |
|---|---|---|
| **1** | Introduce the strategy enums; port Go + Java + Swift (the three that share `def_qn`/`call_entry`) off the trait. | All five parity suites, `graph_accuracy` 41/41, `parser_fidelity` unchanged. |
| **2** | Port Kotlin (navigation-tail callee, label refinement) and Python (three-kind imports, `UPPER_SNAKE` filter, `is_async`) — the two hardest. **Delete `conventions.rs`.** | Same suites, unchanged. Trait no longer exists ⇒ required-method wall gone. |
| **3** | `SpecSource` port + built-in table as the default source + serde. Still no DB. | Suites unchanged; specs now serialisable round-trip (property test). |
| **4** | `DbSpecSource` + tool-global store + first-run seeding + frozen startup registry + degraded-mode signal + guard extended to loaded rows. | Suites unchanged via the built-in path; new tests for load failure / malformed row / missing grammar / signal emission. |
| **5** | Express #97/#98/#99/#100 as spec-data edits (Swift conformance edges, protocol requirements, enum model, accessor call scanning). **Behavior-changing by intent** — new fidelity pins, parity corpus updated deliberately. | New pins; `graph_accuracy` unaffected or improved. |
| **6** | Add Ruby with **zero** walker/conventions code → **closes #60**. | Accuracy-gate parity on a fixture repo, per #60's criterion. |

Steps 1–4 are behavior-preserving; step 5 is the only intentional behavior change and is gated by its own fidelity pins. Each step is one PR with a Completion Ledger.

---

## Consequences

**Positive.** #60's criterion becomes reachable. A language becomes data a non-Rust contributor (or a DB row, or an LLM) can supply. The ~970 LOC of per-language behavior collapses toward data plus one interpreter. Two standing rule violations disappear. The interpretation logic gets five corpora of mutation pressure instead of one each.

**Negative / accepted costs.**
- **A large behavior-preserving refactor across five languages.** Mitigated by the exact-parity suites: this is the ideal refactor to attempt precisely because the safety net already exists and is strict.
- **Compile-time checking is traded for runtime validation** on DB-loaded rows. A typo in a static Rust table is a build error; a typo in a DB row is a startup error. Mitigated by extending the existing `guard.rs` spec validator to loaded rows and failing loudly with a named signal (§13.1 F1/F2) — never silently degrading extraction quality.
- **Strategy-enum proliferation is a real risk.** If every language adds an enum variant, the trait has been re-created under another name. **Tripwire:** a new language that cannot be expressed by the existing variants is a design review, not a reflexive new variant. Recorded as this ADR's Risk #1, the direct successor to ADR-0055's Risk #1 — and the honest lesson is that ADR-0055's watch signal fired five times before being acted on. This one must be acted on at the *first* fire.
- **Grammars still require vendoring** (see boundary above).

**Neutral.** Public output contract (`ParseResult`/`ExtractedNode`/`ExtractedRef`), graph schema, and MCP API are unchanged throughout. The five un-migrated hand-written walkers (Rust, TypeScript, C, C++, Obj-C) are untouched and keep working; they migrate onto the data model as separate, independently gated steps.
