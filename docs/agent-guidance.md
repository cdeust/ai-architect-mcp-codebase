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
- `bin/ensure-binary.sh` pins the SHA-256 of `.claude-plugin/plugin.json` and of the `[package]` table of `Cargo.toml` (not the dependency tables, so a dependency bump leaves it valid). Both drift on a version bump or any `[package]` edit — run `python3 scripts/repin_bootstrap_digests.py` or CI fails.

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
- **`install` reads the repository's latest release before it decides what
  to require** (`scripts/smoke_claude_plugin.py --installed`, the last command
  of `ci.json` `install`). The source is one request to the github.com page
  `https://github.com/cdeust/ai-architect-mcp-codebase/releases/latest`,
  redirects not followed: a `302` whose `Location` ends in
  `/releases/tag/v<X.Y.Z>`. No REST call and no token are used (the anonymous
  REST quota is 60 requests per hour per address, and hosted runners share
  addresses). The latest release is never a draft or a prerelease. Any other
  answer (another status, another `Location`, no network) fails the job: an
  unknown state is never read as "not released".
  - *Plugin version at or below the latest release*: the published contract,
    unchanged. The launcher must download the attested asset, verify its
    checksum and provenance, and answer the exact tool catalog of that tag and
    `health_check`. A missing asset or catalog fails, so a version that was
    skipped or lowered fails here.
  - *Plugin version above the latest release* (a release pull request, and its
    merge until the release is public, draft included): the launcher must fail
    closed: exit 1, one `FATAL: release asset unavailable:
    …/v<version>/…` line, curl's 404, nothing on stdout, no source build, no
    binary under `target/release`. The job then prints one line starting with
    `INSTALL: v<version> is not released yet;`. It proves that the plugin
    installs and refuses; it does not prove the download path. The tool
    catalog and `health_check` of that commit are the `e2e` job's (same
    commit, binary built from source), and the download path is proved by
    `release.yml`'s `publish_verified_release` and by the next `install` run
    after the release.
- CI catches blast radius that local gates miss — activating a resolver path can break a cross-repo defense test. Watch the run, don't assume.

## Etiquette

Conventional commits, staged file-by-file, never `git add -A`. One PR per concern. A pull
request merges when CI is green and the review's last verdict on its head is an approve;
the owner does not gate merges by hand. CI is the authority: it exists to catch
regressions and enforce the engineering standards.

## CI contract for agents and routines

This section binds every agent that works on this repository: an interactive session,
a scheduled routine, the local manager. It follows the contract of `cdeust/Cortex`, the
source of truth for CI. Where the prompt of a routine is less restrictive, this section
wins: the prompt was written before the contract, and the repository's own standards
apply. It never widens what a prompt allows.

1. **The review verdict decides the merge.** The `PR Review` workflow posts a comment by
   `claude[bot]` that ends with `Verdict: approve` or `Verdict: block`. The resolver merges
   a pull request whose last verdict covers its head, reads approve, and whose required
   checks are green. Two cases differ: a Dependabot update the resolver classes as low risk
   (one commit signed by GitHub, minor or patch only, dependency manifests and lock
   files only, nothing under `.github/`) merges on green required checks without a
   review, and a title that contains "do not merge" is never merged. No
   agent merges by hand or enables auto-merge.
2. **Three reviews per pull request.** The budget counts the successful review runs of
   the pull request, so a push to an open pull request spends one. The `claude-rereview`
   label grants a fourth; it is the owner's to set, and the next push uses it. Push once
   per verdict, with every blocker of that verdict fixed, and never while a `PR Review`
   run is in progress on the pull request: the review would land on a commit that is no
   longer the head.
3. **After the third review the pull request is not reviewed again.** What the last
   review accepted merges; what it did not goes to a refinement pull request on a new
   branch, which has a budget of its own. When the head never got an approve, the same
   commits go to `<branch>-r<N>`: open a pull request with the same title and body plus a
   "Continuation" section naming the old one, leave one "Continued in #n" line on the old
   one, close it. A forced push of any kind, `--force-with-lease` included, is never
   used: it does not reset the budget and it rewrites a branch another agent may hold.
4. **No comparison with the main branch.** Do not measure how far a branch is behind, do
   not rebase a pull request branch onto the main branch, do not use the branch-update
   button or its API, and do not write that a pull request is late: a late pull request
   merges. The only exception is a real conflict (`mergeable_state` is `dirty`): merge the
   main branch into the branch with a merge commit, resolve it, rerun the touched tests,
   check that the merge did not change the meaning of either side, and push normally.
5. **One pull request in flight.** A pull request is in flight while a check runs on it, a
   review runs on it, or an approve waits for the merge. While one is in flight, open no
   other pull request (issue, relaunch or next slice) and push to no other branch. A pull
   request under rule 7 is not in flight.
6. **No comment after an approve.** No reply to the notes, no summary, no acknowledgement.
   Notes marked "no action before merge" go to a follow-up pull request after the merge.
   Before an approve, comment only with a pushed fix, a hold, or the continuation line
   of rule 3. Never write a line that begins with `Verdict:`: that line is the review's.
7. **Owner decisions are left alone:** a pull request on the branch
   `ci/budget-counts-contract-reviews`, anything labelled `hold` or `do-not-merge`, a draft.
8. **The work is verified before the pull request exists** (Cortex ADR-1132). Before
   opening a pull request, and before each push to one, run the `unit` and `regression`
   commands of `ci.json` on a clean copy of the commit, a worktree under
   `.claude/worktrees/` (a `git archive` is not one here: tests of the `regression`
   command call git), plus what the change needs beyond them, and put each command with
   its passed, failed and skipped counts in the pull request body.
   A command the session cannot run is named there with the reason. The hosted Pipeline
   stays the gate; it is not the place to find out whether a change works. After a
   conflict merge under rule 4, the touched tests are enough. A changelog or
   documentation entry is pushed with the change it records; documentation is pushed
   alone only to fix a blocker.
9. **A pull request that changes the `ci.json` validator leaves a tree the earlier
   validator still passes** (Cortex ADR-1134). The Pipeline after merge runs the workflow
   of the commit before the merge: its `contract` job validates the merged tree, `ci.json`
   and the version file it names, with the validator of that earlier commit. In Cortex,
   the two pull requests that added a key to `ci.json` together with the validator that
   accepts it each merged, then failed that job, and the test jobs of that run were
   skipped. Such a pull request replays the step before it is opened and before each push
   to it: the validator as it was before its commits, on the tree after them. So a key
   enters `ci.json` in a pull request after the one that taught the validator to accept
   it; what fails without the key, a validator that requires it or a reader that refuses
   its absence, comes with the key or after it; a key the validator requires leaves
   `ci.json` after the validator stopped requiring it.
