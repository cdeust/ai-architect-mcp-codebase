# Contributing to ai-architect-mcp-codebase

Thanks for considering a contribution. This is a Rust MCP server with
**26 tools, 1500+ tests, zero warnings, every constant sourced**. Every
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

cargo test --release        # full test suite (1500+ tests)
cargo clippy --release -- -D warnings   # zero warnings policy
cargo fmt --check
```

The `.mcp.json` shipped at the repo root registers the binary with Claude
Code automatically when you open the directory.

**Iterating on the installed plugin instead of the repo's `.mcp.json`:** if
you develop against a marketplace-installed plugin (so you can dogfood the
exact install path other users hit), the plugin cache's binary digest pin
will reject your rebuilds unless you opt out with
`AI_ARCHITECT_SOURCE_CHECKOUT=1` — see [README §Developer escape
hatch](README.md#developer-escape-hatch-running-a-local-dev-build-in-place-of-the-release)
for the two accepted checkout shapes, what the flag does and does not skip,
and the non-interactive-shell gotcha (`~/.zshenv`, not `~/.zshrc`).

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
cargo test                              # full suite (1441 tests, measured 2026-08-26 on CI/ubuntu)
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
4. **Statement coverage must not regress below 80%.** It is 90.02% (measured
   2026-08-26 by the `cargo llvm-cov (80% line floor)` CI job, which is the
   figure the README badge is gated against). Run it locally before a large
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

### Publishing `server.json` to the MCP Registry

**Automatic.** The `publish_registry` job in `.github/workflows/release.yml`
publishes `server.json` to the official MCP Registry
(`registry.modelcontextprotocol.io`) with the `mcp-publisher` CLI, after
`publish_verified_release` succeeds — it can never run against a `.mcpb`
release asset that does not exist yet or is still a draft. Authentication
uses `mcp-publisher login github-oidc`: the job's GitHub Actions OIDC token
is exchanged for a registry credential scoped to `io.github.cdeust/*`. No
secret is stored or required. Source:
[modelcontextprotocol/registry — GitHub OIDC (CI/CD)](https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/cli/commands.md)
and [publishing from GitHub Actions](https://github.com/modelcontextprotocol/registry/blob/main/docs/modelcontextprotocol-io/github-actions.mdx),
verified 2026-08-10.

`server.json`'s committed `packages[0].fileSha256` is a placeholder
(`"000...0"`) **by design**: that digest is a property of the built `.mcpb`
bundle and cannot be known before it is built. The `publish_registry` job
downloads the public, checksum- and attestation-verified `.mcpb` asset,
computes its real digest, and patches a runtime copy of `server.json`
before publishing — the committed file is never rewritten. `version` and
`packages[0].identifier`, which *are* known ahead of the build, are still
asserted against the tag being published; a stale commit fails the job
loudly instead of publishing wrong metadata.

Before publishing, the job downloads and re-verifies the `.mcpb` asset's
checksum and provenance attestation. After publishing, it queries the
registry's own API and fails the job if the response does not match — a
green `mcp-publisher publish` exit code is not treated as proof.

**Recovery / first-publish path.** If the registry entry is missing or
stale for a tag whose GitHub Release is already public — including the
very first publish under a renamed identity — re-run `publish_registry` via
`workflow_dispatch` on `release.yml` with the `tag` input set to the
existing tag (e.g. `v0.9.1`). This does not rebuild or re-release anything;
it refuses to run without an explicit tag.

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
