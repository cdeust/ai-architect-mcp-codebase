# Assurance case

An assurance case is an argument, not a checklist: a claim about security, the
reasoning that supports it, the evidence each step rests on, and — the part
that makes it honest — where the argument stops.

*Required by OpenSSF Best Practices silver criterion `assurance_case`. It is
the security argument behind [SECURITY.md](../SECURITY.md), which states the
requirements; this document argues that they are met.*

## What this software is, in threat terms

`automatised-pipeline` is a **local MCP server**. An agent host (Claude Code or
another MCP client) starts it over stdio and calls its 26 tools. It parses a
source tree into a LadybugDB property graph, resolves relationships across
files, and answers structural questions about the code.

Three properties shape the whole threat model:

1. **It reads everything.** Indexing walks the entire path it is given —
   source, config, docs, and git history when co-change mining is on. That is
   the product, and it is also exactly the access an attacker would want.
2. **Its input is untrusted by construction.** A repository being indexed is
   attacker-influenced data: a malicious or merely malformed file is a normal
   input, not an exceptional one. Tool arguments arrive from an agent, which is
   itself driven by model output.
3. **It runs with the user's own privileges and writes only where told.** There
   is no privilege boundary between the server and the person running it, so
   the interesting risks are not "can it escalate" but "can it be induced to do
   something the user did not intend" — corrupt its own store, consume
   unbounded resources, or ship them a binary that is not the one we built.

### Trust boundaries

| # | Boundary | What crosses it | Where it is mediated |
|---|---|---|---|
| B1 | Agent host → server | JSON-RPC tool arguments: paths, ids, Cypher-ish queries, qualified names | `validate_safe_id` and `require_absolute` in `src/main.rs`; per-tool required-field checks |
| B2 | Indexed repository → parser | Arbitrary bytes in arbitrary files, any encoding, any size | tree-sitter parsers, size/parse bounds, coverage sidecar |
| B3 | Parsed data → graph store | Symbol names and paths derived from untrusted files | `graph_store::cypher_str` (single escaping choke point) + prepared statements |
| B4 | Server → response | Query results of unbounded natural size | `src/response_budget.rs` |
| B5 | crates.io + GitHub Actions → build | Dependency code, action code | `Cargo.lock`, `deny.toml`, SHA-pinned actions, Dependabot |
| B6 | Release artifact → user | The binary you actually run | SHA-256 per asset today; provenance attestation **not yet on a published release** (see Claim 5) |

There is no network boundary: the crate has no HTTP client, no async runtime,
and no telemetry. Verified rather than assumed — `cargo tree -e normal` resolves
172 crates and none of them is `reqwest`, `hyper`, `ureq`, `curl`, `tokio`,
`async-std`, `rustls` or `openssl` (checked 2026-07-27). The README's dependency
table states the exclusion as a deliberate decision.

## Claim 1 — Untrusted repository content cannot corrupt the graph store through injection

**Argument.** Symbol names, file paths and qualified names come out of files an
attacker may control and go into Cypher statements. String-built queries are the
classic path from "indexed a hostile repo" to "executed attacker Cypher".

**Evidence.**
- The vulnerability was real and is fixed: issue #16 found the naive escaping
  idiom `.replace('\'', "\\'")`, which turns a `\'` payload into an escaped
  backslash followed by an *unescaped* quote — closing the literal early and
  letting the rest run as live Cypher (e.g. `DETACH DELETE n`).
- The replacement is one choke point, `graph_store::cypher_str`, which escapes
  `\` before `'` (source: Neo4j Cypher Manual, "Literals"), with unit tests
  including an explicit injection payload.
- It is pinned mechanically, because a code-review-only defence already failed
  **twice**: `tests/no_naive_cypher_escape.rs` scans every `.rs` file under
  `src/` for the naive pattern and fails `cargo test` if it reappears. That test
  runs on every push and pull request.
- Bulk writes go through prepared statements with `UNWIND` parameters rather
  than concatenated literals.

**Limit.** The guard is a fixed-string scan, not a Cypher-aware analysis: a
*differently* naive escaping helper, written from scratch, would not match its
pattern. It defends against reintroduction of the known idiom, which is the
failure that actually happened, twice.

## Claim 2 — A malformed or hostile file cannot crash or hang the indexer

**Argument.** Parsers are the largest attack surface here: they consume
arbitrary bytes, and the runtime underneath them (tree-sitter) is C.

**Evidence.**
- Two libFuzzer targets, `parse_file` and `parse_file_utf8`, run in CI on every
  pull request (120 s smoke) and nightly (900 s), via `cargo fuzz`
  (`.github/workflows/fuzz.yml`).
- They run under **AddressSanitizer**: `cargo-fuzz`'s `--sanitizer` option
  defaults to `address` (rust-fuzz/cargo-fuzz, `src/options.rs`), and the
  workflow does not override it. The fuzz profile keeps `debug-assertions` and
  `overflow-checks` **on** (`fuzz/Cargo.toml`) so a violated invariant is a
  crash the fuzzer can find, not an optimisation.
- Fuzzing has already produced a fix rather than a report: the OOM in the
  tree-sitter external scanner found under `parse_file` (issue #148) is bounded
  and regression-tested in `tests/parser_parse_bound.rs`.
- A file the parser cannot handle is *recorded*, not silently dropped: the
  coverage sidecar marks it uncovered or parse-partial, and
  `tests/coverage_honesty.rs` asserts that an intentionally corrupt fixture is
  reported while the rest of the repository still indexes cleanly.

**Limit.** Fuzzing covers the two parser entry points. Other ingestion paths —
the IaC readers, the git-history miner — are exercised by the test suite but
not fuzzed.

## Claim 3 — Bounded resources: no single call can exhaust memory or the response channel

**Argument.** Denial of service here is self-inflicted rather than remote: a
query over a large graph can produce a response the host rejects, and an
embedded database that reserves address space per instance can exhaust it.

**Evidence.**
- Responses are budgeted to 100,000 serialized characters, derived (not
  invented) from the host's own cap of 25,000 tokens × 4 chars/token and
  verified against a rejected 324,429-char payload — `src/response_budget.rs`
  carries the derivation and the measurement date.
- `max_db_size` is resolved through one choke point, `graph_store::config`,
  with a measured default (8 GiB, sized from 75 real graphs) instead of lbug's
  8 TiB-per-instance default; an invalid override is rejected with an
  actionable error rather than silently falling back.
- Tests are bounded independently of production through `.cargo/config.toml`'s
  `[env]` table, so the suite cannot mask a production misconfiguration.

**Limit.** These are bounds on *this* process's own behaviour. Indexing a
sufficiently large tree still takes the time and disk it takes; there is no
global quota.

## Claim 4 — Secure design principles are applied at the tool boundary

**Argument.** Saltzer and Schroeder's principles are only worth citing if they
map to code.

**Evidence.**
- **Fail-safe defaults / allowlisting.** `validate_safe_id` accepts only
  `[A-Za-z0-9._-]+`, rejects empty, over-long, leading-`.` and `..` ids;
  `require_absolute` rejects relative paths and any `..` component. Both reject
  by default and enumerate what is allowed, rather than filtering known-bad.
- **Complete mediation.** Validation is at the boundary every tool call crosses
  (the argument parse in `src/main.rs`), not at each call site, so a new tool
  cannot forget it.
- **Economy of mechanism.** One escaping function, one size-config resolver,
  one response budget. Each is a single choke point with its own tests.
- **Least privilege in the project's own infrastructure.** Every workflow
  declares `permissions: contents: read` and elevates only where it must
  (release upload, attestation), which is why Scorecard's `Token-Permissions`
  check scores 10.
- **Memory safety by construction.** Exactly **one** `unsafe` block exists in
  `src/` — a lifetime transmute in `src/graph_store/mod.rs` with a written
  safety argument tied to field drop order. `cargo clippy --workspace
  --all-targets -- -D warnings` is a required check.

**Limit.** Least privilege applies to CI, not to the running server: it reads
with the user's own file-system rights by design, and nothing constrains it to
a subtree beyond the path it is given.

## Claim 5 — What you install is what we built — *not yet true, and stated as such*

**Argument.** A binary distributed as a prebuilt tarball is a supply-chain
target. The user needs to be able to prove which commit produced which bytes.

**Evidence of what is in place.**
- Every GitHub Action is pinned by full commit SHA, and Dependabot covers both
  `cargo` and `github-actions` so a pin cannot quietly rot.
- `cargo audit` and `cargo deny` run **daily** and block on advisories, with no
  blanket ignores: the single accepted entry carries its RUSTSEC id, the reason,
  and the upstream evidence that it cannot be resolved from this repo
  (`deny.toml`).
- The compiler is pinned in `rust-toolchain.toml` and the dependency graph in
  `Cargo.lock`, and the build is **reproducible**: a clean rebuild from the same
  source and target path produced a bit-for-bit identical binary (measured
  2026-07-27, rustc 1.95.0, macOS aarch64).
- Every release asset ships a published SHA-256 companion.
- Provenance attestation and CycloneDX SBOM jobs exist in
  `.github/workflows/release.yml`.

**Where the argument fails today.** Those last jobs were merged **after**
`v0.8.2` was cut. No published release carries an attestation: `gh attestation
verify` against the latest release returns HTTP 404, and OpenSSF Scorecard's
`Signed-Releases` check scores **0**. Until a release is cut from `main`, a user
can verify that the bytes they downloaded match a checksum published on the same
page — which defends against corruption, not against whoever could publish the
page. This is why the silver criterion `signed_releases` is answered **Unmet**
rather than argued around.

## Common implementation weaknesses, and how each is countered

| Weakness class | Countered by | Residual |
|---|---|---|
| Injection (CWE-89/CWE-943 shape, in Cypher) | `cypher_str` choke point, prepared statements, anti-reintroduction test | A newly hand-written escaper would not be caught by the fixed-string scan |
| Path traversal (CWE-22) | `require_absolute` rejects `..`; `validate_safe_id` rejects `.`-leading and `..` ids | The tool still reads whatever absolute path the user names — that is the product |
| Memory-safety defects (CWE-119 family) | Rust; one audited `unsafe` block; ASan fuzzing of the parser entry points | The C/C++ dependencies (tree-sitter runtime, LadybugDB core) are outside our source |
| Uncontrolled resource consumption (CWE-400) | Response budget, `max_db_size` bound, parse bounds from #148 | No global quota on indexing time or disk |
| Silent failure of a check | Coverage sidecar records what it could not parse; the graph-accuracy gate is a blocking CI check with a ratcheting floor | Only the failure modes we have found are instrumented |
| Vulnerable dependency (CWE-1104) | Daily `cargo audit` + `cargo deny`, Dependabot, `Cargo.lock` | One `unsound` advisory is blocked upstream by an exact pin, documented in `deny.toml` |
| Tampered artifact | SHA-256 per asset; attestation machinery merged | **Open** until a release is cut from `main` (Claim 5) |

## What this case does not claim

- **No formal verification.** Correctness rests on tests (947 passing,
  81.59% line coverage measured 2026-07-27), fuzzing, and static analysis.
- **No adversarial review.** Nobody has attempted to attack this software. The
  posture is derived from tooling and reasoning, not from a red team.
- **No multi-party review.** With one maintainer, no change is reviewed by a
  second person — see [GOVERNANCE.md](../GOVERNANCE.md).
- **Nothing about the C/C++ dependencies' internals.** tree-sitter's runtime and
  LadybugDB's core are trusted as upstream code; fuzzing exercises them through
  our entry points but we do not audit them.
- **Nothing about the host.** An agent host that mis-drives the tools, or a
  model that emits a hostile path, is outside this boundary; the server's
  defence is the validation at B1, not an assumption about the caller.
