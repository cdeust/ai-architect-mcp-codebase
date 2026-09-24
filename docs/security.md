# Security model

The server reads source code and answers questions about it. It does not edit
the files it indexes. It writes its graph and sidecars under the `output_dir`
a call names; `index_codebase` with `export_artifact: true` also writes a
snapshot and a `.gitattributes` line into the indexed repository (see
[configuration](configuration.md#team-shared-graph-artifact)). The finding and
PRD-artifact tools write JSON files under their `output_dir`.

The full security argument (threat model, trust boundaries, what each claim
rests on, and where it stops) is in [ASSURANCE-CASE.md](ASSURANCE-CASE.md).
The reporting process and response times are in
[SECURITY.md](../SECURITY.md). This page lists the hardening in place and
explains how `query_graph` stays read-only.

## Contents

- [Hardening](#hardening)
- [How `query_graph` stays read-only](#how-query_graph-stays-read-only)
- [Release verification](#release-verification)

## Hardening

A `security-auditor` agent pass reported four critical, four high and three
medium findings. They were fixed in commit
[`512d683`](https://github.com/cdeust/ai-architect-mcp-codebase/commit/512d683),
and later issues added to the list. Each fix has a test that asserts the
exploit is now rejected.

| Risk | Mitigation |
|---|---|
| Cypher injection when inserting edges | Centralized `cypher_str()` escaping (`\` first, then `'`); hot read lookups bind values as parameters (0.12.0) |
| Git argument injection | `validate_git_ref` rejects `--`, newlines and NUL, and a `--` separator precedes refs |
| Arbitrary binary execution through `lsp_command` | Allowlist of bare names: `rust-analyzer`, `pyright`, `pyright-langserver`, `typescript-language-server` |
| Symlink traversal | `fs::symlink_metadata` plus a maximum walk depth |
| Resource exhaustion | `MAX_FILES = 100_000`, `MAX_FILE_BYTES` = 10 MiB, `MAX_TOTAL_BYTES` = 2 GiB, `MAX_DEPTH = 64` |
| Pathological parser input | A 5-second parse deadline per file (`PARSE_TIMEOUT_MICROS`) and `MAX_PARSE_BYTES` = 1 MiB |
| Writes through `query_graph` | Two layers over disjoint statement families, described below |
| Deleting outside a graph directory | `validate_graph_path_safe()` runs before any `remove_dir_all` |
| LSP `rootUri` injection | RFC 3986 percent-encoding |
| Diff line overflow | `DIFF_LINE_MAX = u64::MAX / 2` guard |
| Forged freshness sidecars | `meta.json` roots and manifest keys are validated before any `stat` (0.12.0, see the CHANGELOG's Security section for the residual) |

## How `query_graph` stays read-only

Two layers cover disjoint statement families, and neither one covers the
other.

| Layer | Refuses | Mechanism |
|---|---|---|
| Engine (`GraphStore::execute_read_only_query`) | Every database write and DDL statement (`CREATE`, `MERGE`, `SET`, `DELETE` and `DETACH DELETE`, `DROP`, `ALTER`), however it is spelled | `PreparedStatement::is_read_only()`: the verdict comes from the compiled plan, so a mutation written in a syntax no keyword scan lists is still refused |
| Lexical (`FORBIDDEN_CYPHER_KEYWORDS`) | Filesystem and database movement: `COPY … TO`, `EXPORT` and `IMPORT DATABASE`, `ATTACH`, `DETACH`, `USE`, `LOAD FROM` | Whole-word, case-insensitive scan of the query after string literals, backticked identifiers and comments are masked out |
| Lexical (`READ_ONLY_PROCEDURES`) | Every `CALL` that names anything except `TABLE_INFO` or `SHOW_TABLES`, including the `CALL <setting> = <value>` configuration form | Per-procedure classification of the identifier after each `CALL` token |

Both lexical constants live in `src/query_handlers/read_only_gate.rs`.

The lexical layer carries real weight. lbug's `StatementReadWriteAnalyzer`
overrides `visitCopyFrom` but leaves six statements at the base visitor's
no-op (`visitCopyTo`, `visitExportDatabase`, `visitImportDatabase`,
`visitAttachDatabase`, `visitDetachDatabase` and `visitUseDatabase`, in
`parsed_statement_visitor.h` lines 51 and 57 to 61 on lbug 0.19.1), so all six
are classified read-only. `DETACH` and `USE` were added to the denylist on
2026-08-25 after a mechanical re-audit against those headers; before that,
both passed the lexical filter and the engine filter. Measured on 2026-08-24
against lbug 0.19.1 on both engine gates available (`is_read_only()`, and a
database opened with `SystemConfig::read_only(true)`, which reaches the same
predicate through `ClientContext::validateTransaction`): `COPY (…) TO 'f.csv'`
and `EXPORT DATABASE 'd'` execute and write to the filesystem, while both
gates refuse `CREATE NODE TABLE`. The tests
`engine_gate_does_not_cover_filesystem_writes` and, for the whole family,
`engine_classifies_every_filesystem_statement_as_read_only` pin this
behaviour. The lexical gate and the engine are also differential-tested
against each other on quoting, escaping and comment grammar, so a change in
lbug's lexer breaks a test.

`CALL` is classified per procedure, so schema introspection
(`CALL table_info('Function') RETURN *`) is allowed. The same analyzer returns
`readOnly = true` from `visitStandaloneCall`, so `CALL threads = 8`, a
configuration write, is read-only to the engine, and the lexical layer is the
only barrier against it. Relaxing the keyword instead of allowlisting the
procedure would remove that barrier.

A keyword introduced by `:` or `.` is an identifier, so queries over this
schema's own `Import` node table work unchanged:
`MATCH (f:File)-[:Defines_File_Import]->(n:Import) WHERE n.is_resolved = false RETURN n.path`.

The gate does not extend that exemption to an alias (`AS <keyword>`), though
the clause detectors do. The asymmetry is deliberate. On the gate, an
exemption can only let a keyword through, so the gate fails closed and refuses
a bare `use` or `create` pattern variable (backtick it). On the clause
detectors the costly direction is reversed, because reading `AS limit` as a
clause would suppress the `LIMIT` injection. A masked literal or backticked
identifier counts as a token, never as whitespace, so no look-back can walk
across one.

`query_graph` runs one statement per call. A trailing `;` is accepted. A
`;`-chained request is refused with reason `multi_statement_not_supported`,
because the read-only classification, the `LIMIT` injection, the `ORDER BY`
detection and the offset cursor are all properties of a single statement.
Queries carry a 30-second bound, applied before prepare, so binding and
planning are bounded too.

## Release verification

Release binaries carry a Sigstore build-provenance attestation and a SHA-256.
The Claude Code plugin bootstrap verifies both before it installs a binary and
pins the digest afterwards; see
[install](install.md#claude-code-plugin) and the
[developer escape hatch](install.md#developer-escape-hatch-running-a-local-dev-build-in-place-of-the-release).

OpenSSF Best Practices answers, criterion by criterion:
[.bestpractices.json](../.bestpractices.json). Governance and continuity:
[GOVERNANCE.md](../GOVERNANCE.md).
