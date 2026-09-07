---
name: validate-change-plan
description: Check whether a proposed implementation plan matches the current code graph. Use before coding or approving a plan that names files, symbols, dependencies, boundaries, or expected blast radius; this is structural plan validation, not full PRD or semantic correctness verification.
---

# Validate Change Plan

Use only the eight tools in the `core` profile. Do not present this workflow as equivalent to the full profile's PRD validator, security gates, or semantic-diff verifier.

For Rust caller or impact questions, request `lsp: true` in `analyze_codebase`
and make rust-analyzer available. Inspect `lsp_status.state`: `disabled` means
static-only analysis; `failed` means analysis continued and may retain partial
LSP results. Record the error and treat affected conclusions as unverified until
source checks or a successful rerun support them. `completed` means the pass
returned, not that every call resolved. Record unresolved sites and coverage gaps.

If a full-profile workflow supplies a `check_security_gates` receipt, inspect
both `gates_passed` and `report.assessment_complete`. A true pass with an incomplete
assessment is not security clearance: report skipped checks, unresolved symbols
and warnings. This core-profile workflow does not invoke that additional tool.

## Workflow

1. Break the plan into falsifiable claims: files exist, symbols exist, dependency directions are correct, named boundaries match communities or processes, and the stated blast radius is plausible.
2. Call `health_check`, then build or refresh the graph with `analyze_codebase`. Reuse the returned `graph_path`.
3. Resolve each named symbol with `search_codebase`, then verify it with `get_symbol` or `get_context`. Mark ambiguous or missing names as unverified instead of choosing a convenient match.
4. Call `get_impact` for every symbol the plan changes. Compare callers, importers, users, implementors, communities, and processes with the plan's scope claims.
5. If a real diff exists, call `detect_changes` and compare its affected symbols and risk scores with the plan. Skip this step for a plan-only review.
6. Call `query_graph` with `graph="missed"`. Inspect flagged ranges and search the relevant source, including unflagged files, before accepting any absence-based claim. A clean coverage report does not prove all relationships were recovered.
7. Use read-only `query_graph` queries for remaining structural assertions, adding `ORDER BY` when paging.

## Verdict

Return one row per plan claim with the evidence, a verdict of `supported`, `contradicted`, or `unverified`, and the smallest correction needed. End with coverage gaps, unresolved questions, likely tests, and the plan steps that require deeper semantic or runtime validation.
