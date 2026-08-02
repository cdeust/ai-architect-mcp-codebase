---
name: impact-analysis
description: Trace the structural blast radius of a proposed or actual code change with ai-architect. Use for implementation planning, pull-request review, regression analysis, dependency checks, or identifying callers, importers, users, communities, and processes affected by a symbol or diff.
---

# Impact Analysis

Use the eight-tool `core` profile. Treat graph results as a lower bound wherever the tool reports heuristic resolution, dynamic dispatch, or coverage gaps.

## Workflow

1. Call `health_check`. Build or refresh the graph with `analyze_codebase`, and keep the exact returned `graph_path`.
2. If an actual unified diff or base/head refs exist, call `detect_changes`. Do not invent a diff for a proposal that has not been implemented.
3. Use `search_codebase` to resolve every proposed or changed symbol to an exact qualified name. Confirm important targets with `get_symbol` or `get_context`.
4. Call `get_impact` for each target. Page callers through `next_offset`. Treat importers, users, and implementors as capped summaries; use `query_graph` with an explicit `ORDER BY` when a complete paged list matters.
5. Call `query_graph` with `graph="missed"`. Inspect flagged files or ranges before making a negative claim about callers, dependencies, or test coverage.
6. Follow high-risk dependents with `get_context` or `get_symbol`. Use sibling graphs only when the user supplies or authorizes those graph paths.

## Deliverable

Separate direct graph evidence from inference. Report affected symbols, callers and dependency types, communities, execution processes, co-change hints, coverage gaps, and unverified risks. Preserve the tool's epistemic qualifier instead of upgrading a lower-bound result into a completeness claim.

