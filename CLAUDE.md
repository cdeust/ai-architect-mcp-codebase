# ai-architect-mcp-codebase

Rust MCP server: parses repositories with tree-sitter, builds a code graph, answers
structural queries.

This file is deliberately short: the host harness (hooks, skills, agents) loads what it
needs on demand. Everything that used to be here is in `docs/agent-guidance.md`
(environment quirks, gates that fail CI but not local runs, etiquette). Read it before any
non-trivial change.

## Commands

```bash
cargo test --lib                                    # unit tests
cargo test --test graph_accuracy                    # accuracy gate
cargo run --release -p bench-end-result --bin bench_end_result -- --all   # note the -p
```

Exit 0 requires aggregate ≥0.85 and every language ≥0.75.

## Non-negotiables

- This clone is live-mounted as the installed plugin: never `checkout`, `pull`, `stash`, or
  build a different branch in it — the running MCP server dies. Work in a worktree:
  `git worktree add .claude/worktrees/<topic> -b <branch> origin/main`.
- `bin/ensure-binary.sh` pins the SHA-256 of `Cargo.toml` and `.claude-plugin/plugin.json` —
  update both together or CI fails.
- CI contract (`docs/agent-guidance.md`, "CI contract for agents and routines"): it binds
  every agent and routine and wins over a less restrictive routine prompt. One pull request
  in flight, no comparison with the main branch, no forced push, no comment after an approve.
- Issues: no issue waits for the owner (`docs/agent-guidance.md`, "Issues: no issue waits
  for the owner"). Decide, write the decision and its reason, deliver; hold only for the
  irreversible, a missing access, a named open dependency or the attempt cap.
- Every commit is signed off (`git commit -s`, the `Signed-off-by` line in the last paragraph
  of the message with the other trailers). The check `every commit signed off` fails
  otherwise, and a pushed commit cannot be rewritten: pull requests 524 and 529 stopped there.
