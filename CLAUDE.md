# ai-architect-mcp-codebase

Rust MCP server: parses repositories with tree-sitter, builds a code graph, answers
structural queries. See @CONTRIBUTING.md for the layer rules and coding standards.

Global rules — model behavior, the loop discipline, and the zetetic standards — are imported,
not restated here:

@~/.claude/rules/model-behavior.md
@~/.claude/rules/coding-standards.md

## Environment quirks — these will cost you a session if ignored

- **This clone is live-mounted as the installed plugin.** `~/.claude/plugins/cache/.../0.9.x/target/release/ai-architect-mcp-codebase` symlinks here. Never `checkout`, `pull`, `stash` or build a different branch in it — the running MCP server dies. Work in a worktree: `git worktree add ../automatised-pipeline-wt-<topic> -b <branch> origin/main`.
- The launcher verifies the release binary's SHA-256 against a pin. A dev rebuild breaks it unless `AI_ARCHITECT_SOURCE_CHECKOUT=1` is exported (it is, in `~/.zshenv`). A dead server shows only as `MCP error -32000: Connection closed`; the real message appears when running `bin/launch-plugin.sh` manually **with `CLAUDE_PLUGIN_ROOT` set**.
- `bin/ensure-binary.sh` pins the SHA-256 of `Cargo.toml` and `.claude-plugin/plugin.json`. Both drift whenever dependencies or version change — update them or CI fails.

## Commands

```bash
cargo test --lib                                    # unit tests
cargo test --test graph_accuracy                    # accuracy gate
cargo run --release -p bench-end-result --bin bench_end_result -- --all   # note the -p
```

The bench archives to `benches/runs/<ts>.md` (gitignored; force-add only a release manifest).
Exit 0 requires aggregate ≥0.85 and every language ≥0.75.

## Gates that fail CI but not local runs

- **Doc-truth**: `scripts/check_doc_claims.py` fails the build when README's test count or coverage badge drifts from measured reality (100-test buckets). Crossing a bucket means updating README **and committing it**.
- **clippy `--all-targets`** compiles `lib` separately from `unittests`: anything reachable only from `#[cfg(test)]` code, imports included, must itself be `#[cfg(test)]`-gated.
- CI catches blast radius that local gates miss — activating a resolver path can break a cross-repo defense test. Watch the run, don't assume.

## Etiquette

Conventional commits, staged file-by-file, never `git add -A`. One PR per concern. Do not merge
your own PR without the owner's go-ahead.
