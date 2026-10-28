# Codebase466 green-wait propagation, 2026-10-28

Base PR head: `5299a3eeff199ceabfc1450237847fb76a8b8ac5`.
Canonical: https://github.com/cdeust/zetetic-team-subagents/pull/205
at `6cb9b18d5068fb1ecf510899b52527fae786b163`.

Two assets copied byte for byte from the reviewed canonical commit:

- `scripts/ci_codex_review_reconcile.py`: SHA256 `53acd5512f5b49c3ce42fb36b496a9e5c09ca74afa9604878a2d4c2fbb07d58e`.
- `scripts/tests/test_ci_codex_review_proof_first.py`: SHA256 `1b046a129447084575e2260b7eac4175342bfdd4fa9ec339d81fc3fb48348403`.

The change allows the green-wait hint to reach the existing complete-proof
policy with unresolved findings. The real discovery/policy matrix covers both
clean and unresolved proof across green-wait, failed readiness and failed merge.
Failed merge/unresolved still uses zero REST calls and queues no rerun.

Validation:

- Exact native glob from ci.json: `python3 -B -m unittest discover -s scripts/tests -p '*codex*.py' -q`: 115 passed.
- Ruff check and format with the Cortex Python configuration: passed.
- Actual Cortex craftsmanship scanner: zero violations for both changed assets.
- Diff inspection: only these assets and this receipt changed. Cargo.lock,
  ci.json and both Pipeline files remain byte-identical to the base. Workflow
  configuration and bundle were preserved. No local Cargo rebuild or model call.

ci.json SHA256: `f9b108f56bc5301933f490e2d097ff1a045c145da095fbc055c2267b01dfcbde`.
Hosted CI remains the full Rust/platform compatibility verdict. Parent reviewed
and preapproved exactly the frozen assets and this propagation receipt.
