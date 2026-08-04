# Privacy Policy — ai-architect-mcp-codebase

**Last updated: 2026-06-19**

ai-architect-mcp-codebase is a local, offline Rust binary that runs entirely on your machine. This policy describes what data it processes, where that data lives, and what — if anything — leaves your machine.

---

## What data it processes

When you call `index_codebase` or `analyze_codebase`, the server reads source files in the directories you explicitly pass to it. It parses them with tree-sitter to extract symbols, imports, and call edges, then writes a LadybugDB property-graph database to the `output_dir` you specify.

The following data is processed locally:

- **Source file contents** — read in memory, parsed, and represented as graph nodes and edges. File contents are not stored verbatim; only structural metadata (symbol names, kinds, line ranges, qualified names) is persisted.
- **Git commit history** — only when you call `index_history`. Author names, timestamps, and commit messages from your local git repository are written into the graph database.

No passwords, tokens, secrets, or environment variables are read. The server does not scan directories you do not pass to it.

---

## Where data is stored

All data is stored exclusively on your local filesystem, at the `output_dir` path you provide in each tool call. Nothing is written outside that directory. You control the location; you control deletion.

The graph database is a directory of LadybugDB files. Deleting that directory permanently removes all indexed data.

---

## What leaves your machine

**Nothing.** ai-architect-mcp-codebase makes no network connections. It has no telemetry, no analytics, no crash reporting, and no license validation. It does not phone home.

The binary communicates exclusively over stdio (JSON-RPC 2.0) with the MCP host that spawned it — typically Claude Desktop or Claude Code. That communication is local inter-process communication on your machine.

---

## Your control

- **Stop indexing at any time** — kill the process. No partial writes persist as corrupt state; the server uses atomic file writes throughout.
- **Delete all data** — remove the `output_dir` you passed to `index_codebase`. Nothing else to clean up.
- **Audit the code** — the full source is at [https://github.com/cdeust/ai-architect-mcp-codebase](https://github.com/cdeust/ai-architect-mcp-codebase). Every dependency is declared in `Cargo.toml` with a documented justification comment.

---

## Contact

Questions about this policy: [admin@ai-architect.tools](mailto:admin@ai-architect.tools)
