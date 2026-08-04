# Contributing to ai-architect-mcp-codebase

Thanks for considering a contribution. This is a Rust MCP server with
**23 tools, 220 tests, zero warnings, every constant sourced**. Every
change is held to that bar.

---

## What this project is

A Rust stdio JSON-RPC server (no SDK — we own the wire). Indexes
Rust / Python / TypeScript codebases via tree-sitter, persists to
LadybugDB (a property-graph engine the project builds from C++ source),
resolves cross-file imports + calls, detects functional communities via
Louvain + C2 repair, traces execution flows, and exposes hybrid
BM25 + sparse TF-IDF + RRF search. Read-only intelligence — never
writes code, never opens PRs. See [README](README.md) for the full
architecture.

---

## Dev setup

**Prerequisites:** Rust 1.95.0 — pinned by `rust-toolchain.toml`, so `rustup` installs and selects it for you — plus CMake (LadybugDB
builds its C++ core from source — ~5 minutes first build, cached after).

```bash
git clone https://github.com/cdeust/ai-architect-mcp-codebase.git
cd ai-architect-mcp-codebase
cargo build --release
# First build: ~5 minutes (compiles LadybugDB C++ core)
# Subsequent builds: <1 second incremental

cargo test --release        # full test suite (220+ tests)
cargo clippy --release -- -D warnings   # zero warnings policy
cargo fmt --check
```

The `.mcp.json` shipped at the repo root registers the binary with Claude
Code automatically when you open the directory.

---

## Branching + workflow

- `main` is the integration branch.
- Branch naming: `feature/<short-slug>`, `fix/<short-slug>`, `docs/<short-slug>`, `tool/<tool-name>` (for new MCP tools).
- One MCP tool per PR when adding new ones. The JSON schema, the handler,
  the receipt-style response shape, and the integration test all in the
  same commit.
- Conventional commit messages preferred. Reference issue numbers in the
  body when applicable.

---

## Coding standards (excerpt)

Standard Rust style + a few project-specific rules. The full bar comes
from [zetetic coding standards](https://github.com/cdeust/zetetic-team-subagents/blob/main/rules/coding-standards.md):

- **No warnings.** `cargo clippy --release -- -D warnings` must pass.
  Allow-attributes need an inline justification.
- **No `unwrap()` / `expect()`** in non-test code unless paired with a
  comment explaining why the option/result is provably non-`None` /
  non-`Err` at that point.
- **No `unsafe`** without an explicit safety comment per
  [Rust's `unsafe` guidelines](https://doc.rust-lang.org/nomicon/).
- **§8 Source discipline.** Every numeric constant ≥3 significant digits
  needs a `// source:` annotation (citation, benchmark path, measured
  data with date, or "provisional heuristic" with a calibration plan).
- **§4.1 File ≤500 lines, §4.2 function ≤50 lines.** Split along concern
  boundaries, not arbitrary line counts.
- **No `Box<dyn Trait>`** when an enum dispatch table works. Reflection
  for control flow is refused per §7.
- **`Result<T, E>` over panics** at every public API boundary.

---

## Adding an MCP tool

Each new tool must:

1. **Have a JSON schema** enforced at the wire (`schemars` derive, validated
   on the dispatch path).
2. **Return a receipt-style response** with timing + counts. No bare
   payloads.
3. **Use reason codes on error.** No cryptic protocol errors that don't
   map to a documented failure mode.
4. **Have an integration test** that exercises the full stdio path
   (request envelope → JSON-RPC frame → handler → response). Unit tests
   alone are insufficient.
5. **Be documented in the README's tool table** + the relevant pipeline
   stage.

Reference: look at `src/handlers/health_check.rs` for the simplest tool
shape, `src/handlers/index_codebase.rs` for the canonical heavy-lifting
pattern, and `src/handlers/get_impact.rs` for the cross-graph query
pattern.

---

## Modifying graph algorithms

Graph code (Louvain, C2 repair, Tarjan SCC, Leiden, BFS process tracing)
is tested against published reference implementations. Changes here:

1. **Cite the source paper.** Modifications relative to canonical
   pseudocode require a `// source:` block explaining the divergence.
2. **Preserve invariants.** Modularity must monotonically increase across
   Louvain rounds. Tarjan SCC must produce a valid topological order on
   the condensation.
3. **Benchmark before + after.** `benchmarks/graph-algorithms/` has the
   reference fixtures. A regression of >5% on any benchmark blocks
   merge unless explicitly justified.

---

## Testing

```bash
cargo test                              # full suite (947 tests, measured 2026-07-28)
cargo test --release                    # same suite, release profile
cargo test --release -- --test-threads=1   # serial mode (debugging flakes)
cargo bench                             # micro-benchmarks
```

The per-stage `tests/*_integration.rs` suites drive the pipeline end to end
against fixture data, and several of them (`tests/artifact_bootstrap.rs`,
`tests/installer.rs`, `tests/temporal_runtime_edges.rs`) spawn a real process
and exercise the wire protocol. These are slow but load-bearing — a wire-level
regression is a regression we ship.

### Testing policy (mandatory)

This is a policy, not a preference. It is what the OpenSSF Best Practices
criteria `test_policy_mandated`, `tests_are_added` and `regression_tests_added50`
are answered against, so it has to be true rather than aspirational.

1. **New functionality ships with tests in the same pull request.** Any change
   that adds a tool, a graph edge kind, a parser lane, or any other behaviour a
   consumer can observe MUST add tests for it to the automated suite. A PR that
   adds behaviour and no test is not ready for review.
2. **Every bug fix ships with a regression test that fails on the pre-fix
   code.** Name it after the failure, not the fix, and reference the issue
   number in the file header — the existing suite does this (see
   `tests/no_naive_cypher_escape.rs` for #16, `tests/coverage_honesty.rs` for
   #57, `tests/parser_parse_bound.rs` for #148).
3. **A defence that only a reviewer enforces will be defeated.** Where a fix can
   be silently reintroduced, add a mechanical guard that fails `cargo test`,
   not a comment asking future readers to be careful.
4. **Statement coverage must not regress below 80%.** It is 81.59% (measured
   2026-07-27 with `cargo llvm-cov --workspace`). Run it locally before a large
   change: `cargo llvm-cov --workspace --summary-only`.

---

## Releasing

**A release is not shipped until its pins move.** Tagging, building the
binaries and cutting the GitHub release deliver nothing to plugin installs
on their own: installs subscribe through `.claude-plugin/marketplace.json`,
and the `version` pinned there is what they resolve. A tag the manifest does
not name reaches nobody, and produces no error anywhere.

Four files carry the version and must move together:

| File | Field |
|---|---|
| `.claude-plugin/marketplace.json` | `metadata.version` + `plugins[].version` |
| `.claude-plugin/plugin.json` | `version` |
| `server.json` | `version` + `packages[].version` |
| `Cargo.toml` | `package.version` |

The release checklist therefore ENDS with:

```bash
# after tagging, before considering the release done
python3 scripts/check_marketplace_pins.py   # must exit 0
```

CI enforces this — the `marketplace-pins` workflow runs on PR/push touching
the manifest and on a weekly cron, because pins go stale by inaction and
inaction never opens a PR. The gate reads the latest local git tag, so it
detects a stale pin even when every manifest agrees with itself.

Source: the 2026-07-25 incident — this repo's manifests both sat at 0.8.0
while `server.json` and the latest tag were at v0.8.2, a three-way split
that shipped v0.8.1 and v0.8.2 to zero installs (#67). The same class in
`cdeust/Cortex` withheld six `zetetic-team-subagents` releases and two
`cortex-viz` releases (cdeust/Cortex#179).

---

## What NOT to do

- Don't introduce a new dependency without justification. The Cargo.toml
  is curated; new crates need an issue discussion or an ADR.
- Don't bypass `cargo clippy`. If clippy is wrong, file an upstream
  issue and add a single `#[allow(...)]` with a comment in the same PR.
- Don't add a tool with no integration test.
- Don't merge a graph-algorithm change without before/after benchmarks.

---

## Code of Conduct

This project follows [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

---

## Reporting security issues

See [`SECURITY.md`](SECURITY.md). The MCP server reads filesystem paths
from untrusted input; any path-traversal or injection issue is
high-priority.

---

## License

MIT. Contributions are licensed under the same. See [`LICENSE`](LICENSE).
The graph-theoretic and IR algorithms used remain attributable to their
original authors; the MIT license covers this implementation.
