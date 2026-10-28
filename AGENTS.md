# ai-architect-mcp-codebase

This is a Rust stdio JSON-RPC server for read-only code graph intelligence.
Read `CLAUDE.md`, `docs/agent-guidance.md` and `CONTRIBUTING.md` before changes or reviews.

## Review guidelines

- Preserve MCP wire schemas, reason codes and receipt-style responses. A new tool must include its schema, handler and integration test together, as required by CONTRIBUTING.md.
- Require the existing source-discipline and before/after benchmark evidence for graph-algorithm changes. Do not replace measured graph behavior with guessed edges or unsupported claims.
- Enforce the documented Rust safety rules and zero-warning policy. Public boundaries return Result; unsafe code and production unwrap/expect require their documented justifications.
