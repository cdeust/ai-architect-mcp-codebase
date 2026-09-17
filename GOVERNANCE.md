# Governance

This document states who decides what, how a decision is made, and what happens
to this project if the current maintainer stops. It exists because a project
that cannot answer those questions is a project you should not depend on — and
because `ai-architect-mcp-codebase` asks for read access to your entire source tree,
which raises the bar on "who is behind this".

It is deliberately honest about scale: **ai-architect-mcp-codebase has one
maintainer.** Several practices below would read as bureaucratic theatre at
that size, so they are described as what they actually are rather than dressed
up as a process.

## Roles and responsibilities

| Role | Who | Responsibilities |
|---|---|---|
| **Maintainer** | [@cdeust](https://github.com/cdeust) | Final say on scope, design and releases. Reviews and merges every change. Triages issues and security reports. Cuts releases and owns the publishing identity (GitHub releases, crates.io, the MCP registry entry, the plugin marketplace pin). |
| **Contributor** | anyone opening a pull request | Proposes changes through the process in [CONTRIBUTING.md](CONTRIBUTING.md). Owns the change through review, including its tests, its benchmark evidence where an algorithm moves, and its documentation. |
| **Security reporter** | anyone | Reports privately per [SECURITY.md](SECURITY.md). Credited in the release notes for the patched version unless they ask not to be. |

There is currently **one person in the maintainer role**. That is a real
limitation, stated plainly rather than hidden behind a plural "the team". It is
why OpenSSF Scorecard's `Code-Review` check scores 0 here and why branch
protection requires a pull request with **zero** required approvals: requiring
an approval on a solo repository would deadlock every merge rather than produce
a review. The dismissal is recorded on the Scorecard alert itself, not argued
away.

## How decisions are made

1. **Anything a consumer can observe** — an MCP tool contract, a JSON schema, a
   graph edge kind, a released artifact — is decided in a pull request, in
   writing, with the reasoning in the description. The PR is the record.
2. **Disagreement is resolved by evidence**: a measurement, a citation, or a
   test that distinguishes the two positions. Where no evidence can settle it,
   the maintainer decides and records why in the PR.
3. **Constants and thresholds need a source.** A number with no provenance —
   a paper, a committed benchmark, or dated measured data — is not merged. This
   is enforced in review, and the existing constants carry their `// source:`
   annotations inline (see `src/response_budget.rs` and
   `src/graph_store/config.rs` for the shape).
4. **Architectural decisions are recorded** as ADRs under
   [`docs/adr/`](docs/adr/) when the decision outlives the PR that made it.
5. **Reversals are cheap and expected.** A decision recorded in a PR can be
   revisited by another PR that says what new information changed it.

## How a change gets in

The full process is in [CONTRIBUTING.md](CONTRIBUTING.md). In short: branch off
`main`, open a pull request, pass CI, and get maintainer review. `main` is
protected — no direct pushes, no force pushes, no deletion — and these checks
are required before merge:

- `cargo test (graph accuracy gate)` — the full suite plus the structural
  graph-accuracy floor in `tests/graph_accuracy.rs`
- `cargo clippy (-D warnings, workspace-wide)` — plus `cargo fmt --all --check`
- `analyze (rust)` and `analyze (actions)` — CodeQL

## Becoming a maintainer

There is no ceremony. A contributor who has landed several non-trivial changes,
engages with review substantively, and wants the role can ask for it by opening
an issue. The maintainer will say yes, or say why not, in that issue.

Adding a second maintainer is actively wanted: it is the single change that
would most improve this project's resilience, and it is the only thing standing
between it and the OpenSSF gold criteria `bus_factor`,
`contributors_unassociated` and `two_person_review`.

## Continuity of access

If the maintainer becomes unavailable:

- **The code cannot be lost.** It is public on GitHub under the MIT licence, and
  every release is a git tag. Any clone is a complete copy of the history, and
  the licence permits anyone to fork and continue without asking permission.
- **The build is reproducible without us.** The compiler is pinned in
  [`rust-toolchain.toml`](rust-toolchain.toml) and the dependency graph in
  `Cargo.lock`, so a fork can rebuild the exact artifact from the exact source.
- **The issue history survives.** Issues and pull requests are public and
  archived on GitHub; the reasoning behind each decision lives in the PR that
  made it, not in one person's memory.
- **What is genuinely single-owner** is the GitHub repository itself, the
  crates.io ownership of `ai-architect-mcp-codebase`, the MCP registry entry
  `io.github.cdeust/ai-architect-mcp-codebase`, and the release-publishing identity.
  If the maintainer disappeared without transferring them, the practical
  continuation path is a fork under new ownership publishing under its own
  identity.

That last point is a real single point of failure, and pretending otherwise
would defeat the purpose of writing this down. It is mitigated in the only ways
available to a one-person project — everything needed to continue is public,
licensed for reuse, and independently rebuildable — and it is fully resolved
only by a second maintainer.

## Contribution licensing

Contributions are accepted under the project's [MIT licence](LICENSE), and
every commit must be signed off under the
[Developer Certificate of Origin](https://developercertificate.org/)
(`git commit -s`). The sign-off states that the contributor wrote the change or
otherwise has the right to submit it under the MIT licence. It is tracked in the
source history and easy to verify. It does not assign rights: each contributor
keeps the copyright on their contribution. There is no CLA.

The `DCO` workflow checks every commit of a pull request for a `Signed-off-by:`
trailer matching the commit author. The only exemption is for pull requests
opened by a GitHub login listed in `.github/workflows/dco.yml`, which today
holds `dependabot[bot]`. It keys on the login that opened the pull request;
the git author name, which anyone can set, is only a second condition.
The requirement applies from the change that introduced it; earlier history is
the maintainer's own work and is not rewritten.

## Code of conduct

Participation is governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). The
maintainer is responsible for enforcing it.
