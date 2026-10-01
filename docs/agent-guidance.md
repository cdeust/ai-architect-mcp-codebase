# Agent guidance for ai-architect-mcp-codebase

This is the former body of `CLAUDE.md`, moved here on 2026-09-08 so that it is read on
demand instead of being re-sent to the model on every turn (owner correction: CLAUDE.md
stays nearly empty; the host harness loads what it needs when it needs it). Nothing was
removed except the dangling "Global rules are imported, not restated here" sentence, whose
import lines were already removed in an earlier commit — with no lines left to import, the
sentence had nothing to point at.

See @CONTRIBUTING.md for the layer rules and coding standards.

## Environment quirks — these will cost you a session if ignored

- **This clone is live-mounted as the installed plugin.** `~/.claude/plugins/cache/.../0.9.x/target/release/ai-architect-mcp-codebase` symlinks here. Never `checkout`, `pull`, `stash` or build a different branch in it — the running MCP server dies. Work in a worktree: `git worktree add .claude/worktrees/<topic> -b <branch> origin/main`.
- The launcher verifies the release binary's SHA-256 against a pin. A dev rebuild breaks it unless `AI_ARCHITECT_SOURCE_CHECKOUT=1` is exported (it is, in `~/.zshenv`). A dead server shows only as `MCP error -32000: Connection closed`; the real message appears when running `bin/launch-plugin.sh` manually **with `CLAUDE_PLUGIN_ROOT` set**.
- `bin/ensure-binary.sh` pins the SHA-256 of `Cargo.toml` and `.claude-plugin/plugin.json`. Both drift whenever dependencies or version change — update them or CI fails.

## Commands

```bash
cargo test --lib                                    # unit tests
cargo test --test graph_accuracy                    # accuracy gate
cargo run --release -p bench-end-result --bin bench_end_result -- --all   # note the -p
```

The bench archives to `benches/runs/<ts>.md` (gitignored; force-add only a release manifest).
Exit 0 requires aggregate ≥0.85 and every language ≥0.75. The runner rebuilds the server binary first and the `rust-self` corpus is pinned to a commit (`git_rev`), see `benches/README.md`.

## Gates that fail CI but not local runs

- **Doc-truth**: `scripts/check_doc_claims.py` fails the build when README's test count or coverage badge drifts from measured reality (100-test buckets). Crossing a bucket means updating README **and committing it**.
- **clippy `--all-targets`** compiles `lib` separately from `unittests`: anything reachable only from `#[cfg(test)]` code, imports included, must itself be `#[cfg(test)]`-gated.
- CI catches blast radius that local gates miss — activating a resolver path can break a cross-repo defense test. Watch the run, don't assume.

## Local proof gates

`scripts/gate.sh commit` (fmt, clippy, `cargo test --lib`) and `scripts/gate.sh push` (those, plus
plain `cargo test` with `check_doc_claims.py`, the bench, and the dy-wcet oracle when the diff
touches `src/**/*.rs`) write a proof `<kind>-<tree>.json` to `$(git rev-parse --git-common-dir)/zetetic-gates/`;
a failure writes no proof and one line per failing gate to `failures.jsonl`. `verify [commit|push]` only reads the
proof; `waive <gate> --reason "..."` appends an auditable line and never grants a proof. Docs-only
changes get a proof without cargo (not `README.md`, `skills/`, `plugins/`, `docs/ASSURANCE-CASE.md`: CI parses those with python). Bench and oracle are local and never run in CI; the oracle needs
`GATE_ORACLE_CORPUS` (dy-wcet v4.1.6 checkout, outside any Cargo workspace: under a parent `Cargo.toml` LSP resolution drops and the replay reports false deviations). Nothing compiles in the main clone. Tests:
`bash scripts/tests/test_gate.sh`; mutants: `bash scripts/tests/mutation_check.sh`.

## Etiquette

Conventional commits, staged file-by-file, never `git add -A`. One PR per concern. A pull
request merges when CI is green and a review verdict is posted on it; the owner does not
gate merges by hand. CI is the authority: it exists to catch regressions and enforce the
engineering standards.
