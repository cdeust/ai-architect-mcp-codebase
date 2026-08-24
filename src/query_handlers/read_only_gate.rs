//! `query_graph`'s read-only gate: the lexical layer that decides whether a
//! caller-supplied Cypher statement may reach the engine at all.
//!
//! Split from `query_handlers` when that file crossed the §4.1 500-line cap.
//! Everything here is a pure function over the query text — no filesystem, no
//! graph, no engine — which is what lets the gate be exhaustively tested (see
//! the differential-mutant note on `mask_non_executable`).

use super::is_ident_char;

// ---------------------------------------------------------------------------
// Read-only Cypher filter — source: H3 fix.
//
// Rejects any query that contains a mutation or side-effectful keyword as a
// whole-word, case-insensitive match. This is a conservative allowlist-by-
// blocklist: the engine still validates syntax, we just refuse to hand it
// anything that could mutate state, load external data, or call procedures.
//
// COPY/EXPORT/IMPORT/ATTACH/LOAD (fleet-watch#15): these data-movement
// statements MUST be blocked here, because no engine-side gate catches them.
// lbug's `StatementReadWriteAnalyzer` classifies a statement as read-only from
// the compiled plan, and it overrides `visitCopyFrom` while leaving
// `visitCopyTo`, `visitExportDatabase`, `visitImportDatabase` and
// `visitAttachDatabase` at the base visitor's no-op — so all four come back
// read-only. Measured 2026-08-24 against lbug 0.19.1, on BOTH available engine
// gates: `PreparedStatement::is_read_only()` and a database opened with
// `SystemConfig::read_only(true)` (which reaches the same predicate via
// `ClientContext::validateTransaction`) each EXECUTE
// `COPY (..) TO 'f.csv'` and `EXPORT DATABASE 'd'`, writing the filesystem,
// while both refuse `CREATE NODE TABLE`. `LOAD FROM 'f'` likewise reads an
// arbitrary file. This lexical gate is therefore the only barrier against an
// attacker-named file write or read; the engine gate covers the disjoint
// family of database mutations reachable through syntax this scan misses.
// Pinned by `engine_gate_does_not_cover_filesystem_writes`.
//
// Identifier positions are exempt (see `contains_keyword_token`): `IMPORT` is
// also this schema's `Import` node table, so a keyword introduced by `:` or
// `.` is a label, relationship type, map key or property, not a clause.
// ---------------------------------------------------------------------------

pub(crate) const FORBIDDEN_CYPHER_KEYWORDS: &[&str] = &[
    "CREATE", "DELETE", "MERGE", "SET", "REMOVE", "DROP", "ALTER", "CALL", "LOAD", "COPY",
    "EXPORT", "IMPORT", "ATTACH",
];

/// Wall-clock bound (milliseconds) applied to a single `query_graph` execution
/// and then reset. Bounds a pathological read plan — e.g. an unbounded
/// variable-length `MATCH (a)-[*]->(b)` whose `LIMIT` caps rows, not traversal
/// work — so it cannot pin the single-threaded MCP worker. The MCP server is
/// mono-thread, so one unbounded query freezes every subsequent request; the
/// row-`LIMIT` and byte-page guards bound the RESULT, not the engine's
/// traversal effort.
///
/// source: provisional heuristic — a legitimate interactive graph query
/// returns in well under a second on the corpora this server indexes; 30_000 ms
/// is a generous ceiling only a pathological plan reaches. Operator-tunable if
/// a real workload is found to need longer (calibration: raise only against a
/// measured legitimate query that exceeds it).
pub(crate) const READ_QUERY_TIMEOUT_MS: u64 = 30_000;

/// Returns the first forbidden keyword found in `query`, or None if the query
/// is safe. Matching is whole-word, ASCII case-insensitive, and considers only
/// *executable* text: string literals, backtick-quoted identifiers and comments
/// are masked out first, and a keyword reached through `.` is a property name,
/// not a clause.
///
/// Why (issue #200): whole-word matching alone was not enough. A client that
/// looks a symbol up BY NAME — which is exactly what Cortex's process-symbols
/// path does during ingestion — sends `... WHERE s.name = 'load'`, and the
/// literal tripped the gate. `set`, `create`, `delete` and `call` are ordinary
/// function names in any codebase, so this rejected legitimate read queries
/// against any corpus containing them (observed 6x on automatised-pipeline and
/// 4x on cortex-viz in the 2026-08-06 A/B ingestion bench).
///
/// Masking is safe because nothing masked can execute: text inside a literal or
/// a comment is data, and `n.set` addresses a property. An UNTERMINATED literal
/// is the one case that could hide a real mutation from the scan, so it is
/// rejected outright (fail closed) rather than masked to end-of-input.
pub(crate) fn forbidden_cypher_keyword(query: &str) -> Option<&'static str> {
    let Some(executable) = mask_non_executable(query) else {
        // Unterminated literal/comment — refuse rather than scan a truncated
        // view of the query. Reported as its own sentinel so the caller still
        // gets an actionable read_only_query_required error.
        return Some("UNTERMINATED_LITERAL");
    };
    let upper = executable.to_ascii_uppercase();
    FORBIDDEN_CYPHER_KEYWORDS
        .iter()
        .find(|&&kw| contains_keyword_token(&upper, kw))
        .copied()
}

/// Replaces every non-executable region of `query` with spaces, preserving byte
/// length. Returns None when a literal or block comment is left unterminated.
///
/// Documented equivalent mutants (coding-standards.md §12.4). The scoped
/// cargo-mutants run over this function accounts for all 76 mutants: 56 caught,
/// 10 timed out, 10 survive. Every survivor is an off-by-one on one of the
/// bounds guards below (`<`→`<=`, `+`→`-`, `+`→`*`) or `&&`→`||` in the
/// comment-close check. They are unobservable: each only changes behaviour at a
/// position where the literal or comment is already unterminated, and every
/// such path converges on the same result (return None).
///
/// Established DIFFERENTIALLY, not by argument — reasoning got this wrong once
/// already. The original and each variant were run over EVERY string of length
/// 0..=7 drawn from the alphabet that drives these branches
/// (`'` `"` `` ` `` `\` `/` `*` `a` `\n`), 2,396,745 inputs each, comparing
/// outputs. The survivors listed above show 0 differing inputs. Three OTHER
/// mutants on the block-comment arm showed 10,751 / 7,326 / 2,394 differing
/// inputs — genuinely non-equivalent, and now killed by
/// `readonly_gate_block_comment_boundaries` and
/// `readonly_gate_handles_non_executable_regions_at_index_zero`. The
/// discriminating cases were `/**/`, `/*`, `/*/` and any region starting at
/// index 0, where `i - 1` underflows and `i * 1` coincides with `i`.
/// # source: measured 2026-08-07, differential harness /tmp/exh200b.rs.
///
/// The 10 timeouts stop the index advancing so the loop never terminates; a
/// hang is detection, not an escape.
pub(crate) fn mask_non_executable(query: &str) -> Option<String> {
    let b = query.as_bytes();
    let mut out = vec![b' '; b.len()];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1; // opening quote already blanked
                let mut closed = false;
                while i < b.len() {
                    // Backslash escapes the next byte inside quotes.
                    if b[i] == b'\\' && q != b'`' && i + 1 < b.len() {
                        i += 2;
                        continue;
                    }
                    if b[i] == q {
                        i += 1;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return None;
                }
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i += 2;
                let mut closed = false;
                while i + 1 < b.len() {
                    if b[i] == b'*' && b[i + 1] == b'/' {
                        i += 2;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return None;
                }
            }
            c => {
                out[i] = c;
                i += 1;
            }
        }
    }
    // Masked bytes are ASCII spaces and kept bytes came from a valid UTF-8
    // string at boundaries the ASCII-only matches never split.
    String::from_utf8(out).ok()
}

/// Whole-word match that additionally rejects *identifier* positions: a
/// keyword reached through `.` or `:` names a property, a node label, a
/// relationship type or a map key — never a clause. `n.set` is a property,
/// and `(i:Import)` is this schema's own `Import` node table.
pub(crate) fn contains_keyword_token(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let nbytes = needle.as_bytes();
    if nbytes.is_empty() || bytes.len() < nbytes.len() {
        return false;
    }
    let mut i = 0;
    while i + nbytes.len() <= bytes.len() {
        if &bytes[i..i + nbytes.len()] == nbytes {
            let left_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let right = i + nbytes.len();
            let right_ok = right == bytes.len() || !is_ident_char(bytes[right]);
            if left_ok && right_ok && !preceded_by_identifier_sigil(bytes, i) {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// True when `query` carries more than one statement, i.e. a `;` with
/// executable text after it. Semicolons inside string literals, backticked
/// identifiers and comments do not count — `mask_non_executable` blanks those
/// first, which is what makes this an exact split rather than a guess.
///
/// Why (fleet-watch review finding 9): `execute_read_only_query` must
/// `prepare` the statement to obtain the engine's read-only classification,
/// and `prepare` refuses a chain ("We do not support prepare multiple
/// statements", measured 2026-08-24 on lbug 0.19.1). Before the engine gate
/// existed the request went to `Connection::query`, which accepted a chain and
/// returned the LAST statement's rows — while this tool's LIMIT injection
/// appended to that last statement and its ORDER BY detection scanned the
/// whole chain. Rather than leak the engine's message, name the contract:
/// one statement per call. An unterminated literal is reported as a chain
/// here and refused by the keyword gate upstream, so it cannot slip through.
pub(crate) fn is_multi_statement(query: &str) -> bool {
    let Some(executable) = mask_non_executable(query) else {
        return true;
    };
    match executable.split_once(';') {
        Some((_, rest)) => !rest.trim().is_empty(),
        None => false,
    }
}

/// True when the token starting at `start` is introduced by `.` or `:`,
/// skipping intervening whitespace (`(i : Import)` is legal Cypher).
///
/// Why (fleet-watch review finding 1): the scan previously looked only at the
/// byte immediately left of the match and only for `.`, so `MATCH (i:Import)`
/// tripped the `IMPORT` entry and every query over this schema's `Import` node
/// table was refused — including the `MATCH (f:File)-[:Defines_File_Import]->
/// (n:Import)` shape the accuracy corpora in `benches/corpora/*/ground_truth
/// .json` are written against. Neither sigil can introduce a clause: after `:`
/// Cypher expects a label, a relationship type or a map key, and after `.` a
/// property name, so treating both as identifier positions loses no coverage
/// of the statements this gate exists to refuse.
fn preceded_by_identifier_sigil(bytes: &[u8], start: usize) -> bool {
    let mut j = start;
    while j > 0 && bytes[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    j > 0 && matches!(bytes[j - 1], b'.' | b':')
}

#[cfg(test)]
#[path = "read_only_gate_tests.rs"]
mod tests;
