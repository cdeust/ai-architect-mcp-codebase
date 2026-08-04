# Roadmap

What this project intends to do, and intends **not** to do, over the next year.
It is a statement of direction, not a commitment to dates: a roadmap exists so
that a potential user or contributor can tell whether the project is heading
somewhere they want to depend on.

*Required by OpenSSF Best Practices silver criterion `documentation_roadmap`.
Status of what already works is in the README's
[Status](../README.md#status) section; what shipped is in
[CHANGELOG.md](../CHANGELOG.md).*

## 1. Graph fidelity before graph breadth

The product claim is that an agent can trust the graph more than it can trust a
grep. That claim is only as good as the measured accuracy of the edges, so
fidelity work outranks new languages and new tools.

- Keep ratcheting the structural accuracy floor in `tests/graph_accuracy.rs`.
  The gate is already blocking in CI; each parser fix raises the floor rather
  than adding an exception.
- Close the per-language resolution gaps surfaced by the head-to-head eval
  (`benchmarks/eval_headtohead/`), one language lane at a time, with the eval
  re-run as the evidence rather than a claim in a PR description.
- Extend `Uses` / call-edge extraction to the constructs each language lane
  still misses, implemented once at the `LangSpec` level so adopting it for the
  next language is data, not a new code path.

## 2. Structure: finish the size-cap split

`src/main.rs` (~6500 lines) and `src/graph_store.rs` are far over this
project's own 500-line file cap. The split is tracked as an epic
([#151](https://github.com/cdeust/ai-architect-mcp-codebase/issues/151)) and is a
prerequisite for most other work in this list, because a 6500-line composition
root is where changes go to become risky. Behaviour-preserving, proven by the
existing suite passing unchanged.

## 3. Supply chain: make the assurance real, not just wired

The provenance and SBOM machinery is merged, but **no published release carries
it yet** — the attestation and CycloneDX jobs landed after `v0.8.2` was cut, so
`gh attestation verify` on today's latest release returns 404 and OpenSSF
Scorecard's `Signed-Releases` check scores 0. Closing that is the next release's
job, not a future project.

- Cut a release from `main` so every asset ships with a Sigstore
  build-provenance attestation, a SHA-256, and the CycloneDX SBOM.
- Sign the version tags themselves (annotated + signed), so the source side of
  the release is verifiable independently of the artifacts.
- Keep the daily `cargo audit` / `cargo deny` gate blocking, and keep the
  advisory ignore list at zero blanket entries — an accepted advisory carries
  its RUSTSEC id and a written reason or it is not accepted.

## 4. Assurance: keep the honest posture measurable

- Hold statement coverage at or above 80% and make it a **gate** rather than a
  measurement taken by hand (it is 81.59% today, measured, but nothing stops it
  regressing).
- Machine-check the numeric claims in the README (test count, tool count) the
  way the release pins are already machine-checked by
  `scripts/check_marketplace_pins.py`, so documentation drift fails CI instead
  of aging quietly.
- Grow the fuzz corpus beyond the two parser entry points as new untrusted
  input surfaces appear.

## 5. Reach silver, then take gold as far as one maintainer can

The OpenSSF Best Practices answers live in
[`.bestpractices.json`](../.bestpractices.json). Silver is the target; gold is
partly unreachable at this size, and that is stated rather than worked around —
`bus_factor`, `contributors_unassociated` and `two_person_review` all need a
second maintainer. See [GOVERNANCE.md](../GOVERNANCE.md).

## What this project will not do

These are decisions, not omissions. They will not change in the next year
without an ADR that says what new information changed them.

- **It will not write code.** No edits, no PRs, no CI runs. It is read-only
  intelligence: it tells the system what is true about the code so another
  stage can act. Rename and refactor tools are out of scope for this reason.
- **It will not generate PRDs.** That belongs to
  [prd-spec-generator](https://github.com/cdeust/prd-spec-generator), which
  consumes this graph. One pipeline stage, one owner.
- **It will not call the network at index time.** All processing is local, and
  a tool that reads your entire source tree keeping no network egress is a
  security property worth more than any feature it would buy.
- **It will not grow an LLM or embedding-model runtime.** Sparse TF-IDF plus
  BM25 plus RRF is the retrieval stack; adding a model runtime would add a
  dependency surface disproportionate to the gain.
- **It will not add a dependency without justification.** New crates need an
  issue discussion or an ADR, per [CONTRIBUTING.md](../CONTRIBUTING.md).
