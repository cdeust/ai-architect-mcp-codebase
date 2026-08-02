---
name: understand-codebase
description: Build an evidence-backed map of an unfamiliar repository with the ai-architect code graph. Use before implementing a feature, debugging an unfamiliar subsystem, explaining architecture, or making structural claims about symbols and dependencies.
---

# Understand Codebase

Use the eight-tool `core` profile. Do not assume that pipeline-only tools are available.

## Workflow

1. Call `health_check` and record the active profile and tool count. Stop with an installation or configuration instruction if the server is unavailable.
2. Resolve the repository root to an absolute path. Choose a task-scoped, absolute, writable `output_dir` for graph artifacts; never use the source root or a broad system directory as that output.
3. Call `analyze_codebase` with the repository path and output directory. Reuse the exact `graph_path` returned by the tool in every later call.
4. Immediately call `query_graph` with `graph="missed"`. Treat parse-incomplete, skipped, and quarantined files as explicit coverage gaps. Absence from this report is not proof of complete indexing.
5. Use `search_codebase` to discover qualified symbol names. Drill into relevant hits with `get_context` or `get_symbol`; do not guess qualified names.
6. Use read-only `query_graph` queries for structural questions the typed tools do not answer. Add `ORDER BY` before paging with `next_offset`.
7. For any important negative result, inspect the coverage report and use ordinary file search on the flagged files or ranges before concluding that a symbol or edge does not exist.

## Deliverable

Return a compact map of entry points, components, important symbols, dependencies, and execution flows. Tie every structural claim to a tool result. Label interpretations as inferences and list uncovered files or unresolved references as limitations.

