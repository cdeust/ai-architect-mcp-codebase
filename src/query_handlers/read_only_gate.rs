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
// DETACH/USE (2026-08-25 mechanical re-audit): the same base visitor also
// no-ops `visitDetachDatabase` and `visitUseDatabase`
// (parsed_statement_visitor.h:60-61 on lbug 0.19.1), so `DETACH DATABASE x` and
// `USE DATABASE x` are engine-read-only too. The list stopped at ATTACH and
// those two passed BOTH gates. Re-derived from the headers rather than from
// this comment's own prose, which is how the gap was found; pinned by
// `readonly_gate_blocks_detach_and_use_database` and by
// `engine_classifies_every_filesystem_statement_as_read_only`.
//
// Identifier positions are exempt (see `contains_keyword_token`): `IMPORT` is
// also this schema's `Import` node table, so a keyword introduced by `:` or
// `.` is a label, relationship type, map key or property, not a clause.
// ---------------------------------------------------------------------------

pub(crate) const FORBIDDEN_CYPHER_KEYWORDS: &[&str] = &[
    "CREATE", "DELETE", "MERGE", "SET", "REMOVE", "DROP", "ALTER", "LOAD", "COPY", "EXPORT",
    "IMPORT", "ATTACH", "DETACH", "USE",
];

/// Procedures `query_graph` admits, so an agent can read a graph's SCHEMA
/// without a second tool. Classification is PER PROCEDURE — never by the `CALL`
/// keyword.
///
/// Why per procedure. lbug's `StatementReadWriteAnalyzer` returns
/// `readOnly = true` from `visitStandaloneCall`, so `CALL threads = 8` — a
/// configuration write — is engine-read-only. Relaxing `CALL` wholesale would
/// leave NO barrier anywhere against it. Naming the callable procedures instead
/// keeps the default refusal and admits exactly the two catalog readers.
///
/// source: lbug 0.19.1 `src/include/function/table/simple_table_function.h` —
/// `TABLE_INFO` (line 102) and `SHOW_TABLES` (line 78). The same file declares
/// 24 further procedures (`STORAGE_INFO`, `FSM_INFO`, `SHOW_CONNECTION`, …);
/// none is admitted, because none is needed to answer "what does this graph's
/// schema look like?" and each would need its own read-only argument.
pub(crate) const READ_ONLY_PROCEDURES: &[&str] = &["TABLE_INFO", "SHOW_TABLES"];

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
    if let Some(kw) = FORBIDDEN_CYPHER_KEYWORDS
        .iter()
        .find(|&&kw| contains_keyword_token(&upper, kw))
        .copied()
    {
        return Some(kw);
    }
    // `CALL` is classified per procedure rather than refused outright, so
    // schema introspection is reachable while every other procedure — and the
    // `CALL <setting> = <value>` configuration form, which the engine calls
    // read-only — stays refused.
    if carries_unlisted_procedure(&upper) {
        return Some("CALL");
    }
    None
}

/// True when `executable_upper` carries a `CALL` naming anything outside
/// [`READ_ONLY_PROCEDURES`].
///
/// The `CALL <setting> = <value>` form yields its left-hand side, which is
/// never allowlisted, so it is refused like any unknown procedure. A `CALL`
/// followed by no identifier at all yields the empty name and is refused too.
fn carries_unlisted_procedure(executable_upper: &str) -> bool {
    let bytes = executable_upper.as_bytes();
    keyword_token_positions(executable_upper, "CALL", IdentifierExemption::SigilOnly)
        .into_iter()
        .any(|start| {
            let name = procedure_name_at(bytes, start + "CALL".len());
            !READ_ONLY_PROCEDURES.contains(&name.as_str())
        })
}

/// The identifier following a `CALL` token, or an empty string when none does.
fn procedure_name_at(bytes: &[u8], mut j: usize) -> String {
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    let start = j;
    while j < bytes.len() && is_ident_char(bytes[j]) {
        j += 1;
    }
    String::from_utf8_lossy(&bytes[start..j]).into_owned()
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
/// Stand-in byte for a masked TOKEN — a string literal or a backtick-quoted
/// identifier.
///
/// Comments are blanked to spaces because a comment IS a separator: `ORDER
/// /*c*/ BY` declares an ordering, and `AS /*c*/ limit` aliases. A literal is
/// NOT a separator, it is a value occupying a token position, and blanking it
/// to whitespace is what let three defects through at once:
///
///   * a look-back for `AS` walked across a backticked alias as if it were
///     spaces, so `WITH n AS \`m\` DELETE m` passed the read-only gate and
///     `WITH 1 AS \`x\` CALL storage_info()` shielded CALL from the procedure
///     allowlist — the only barrier that exists for a standalone CALL;
///   * the same walk hid a REAL `LIMIT` after a backticked alias, so a valid
///     bounded query got a second LIMIT injected and the engine rejected it;
///   * `trim_end` over the masked view ate a trailing literal, so
///     `RETURN n.name, 'tag'` was truncated to `RETURN n.name,`.
///
/// One byte that is neither whitespace nor an identifier character keeps token
/// boundaries intact while making "there was something here" visible. A raw
/// `\x01` arriving in executable position can only make a scan MORE strict
/// (it blocks an exemption walk), so the substitution fails closed.
pub(crate) const MASKED_TOKEN: u8 = 0x01;

/// True for a byte that separates tokens: real whitespace, or a masked comment
/// (which this module blanks to a space precisely so it reads as one).
pub(crate) fn is_separator(b: u8) -> bool {
    b.is_ascii_whitespace()
}

pub(crate) fn mask_non_executable(query: &str) -> Option<String> {
    let b = query.as_bytes();
    let mut out = vec![b' '; b.len()];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                let token_start = i;
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
                // A literal or backticked identifier is a TOKEN, not a gap.
                for slot in out.iter_mut().take(i).skip(token_start) {
                    *slot = MASKED_TOKEN;
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

/// Byte positions where `needle` occurs in `haystack` as a standalone keyword
/// token: bounded by non-identifier bytes on both sides, and NOT introduced by
/// `.` or `:` — those make it a property, a node label, a relationship type or
/// a map key, never a clause. `n.set` is a property and `(i:Import)` is this
/// schema's own `Import` node table.
///
/// ONE implementation for every keyword-boundary scan in the crate. There were
/// four, written independently, and two of them — `has_limit_clause` and
/// `has_order_by_clause` — had the word-boundary half but not the sigil half.
/// `WHERE n.limit > 0` therefore read as a declared LIMIT and suppressed the
/// injection that bounds the query, which is the same unbounded row-flood the
/// masking fix had just closed from the other side. Four copies of one rule is
/// how half of them come to carry half of it.
///
/// `haystack` must already be case-folded to `needle`'s case.
/// Which identifier positions a scan treats as exempt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentifierExemption {
    /// `.` and `:` only — the READ-ONLY GATE's setting.
    ///
    /// The gate deliberately does NOT exempt an alias. An exemption can only
    /// ever let a keyword through, so on this scan a false refusal costs an
    /// exotic query a rename while a false exemption costs the gate itself:
    /// `WITH 1 AS x CALL storage_info()` would shield CALL from the procedure
    /// allowlist, and the lexical layer is the only barrier that exists for a
    /// standalone CALL.
    SigilOnly,
    /// `.`, `:` and `AS` — the CLAUSE DETECTORS' setting, where the expensive
    /// direction is reversed: reading `AS limit` as a declared clause
    /// suppresses the LIMIT injection, and reading `AS order` as one
    /// advertises a cursor-safe page that is not.
    SigilOrAlias,
}

pub(crate) fn keyword_token_positions(
    haystack: &str,
    needle: &str,
    exemption: IdentifierExemption,
) -> Vec<usize> {
    let bytes = haystack.as_bytes();
    let nbytes = needle.as_bytes();
    let mut found = Vec::new();
    if nbytes.is_empty() || bytes.len() < nbytes.len() {
        return found;
    }
    let mut i = 0;
    while i + nbytes.len() <= bytes.len() {
        if &bytes[i..i + nbytes.len()] == nbytes {
            let right = i + nbytes.len();
            let left_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let right_ok = right == bytes.len() || !is_ident_char(bytes[right]);
            if left_ok && right_ok && !in_identifier_position(bytes, i, exemption) {
                found.push(i);
            }
        }
        i += 1;
    }
    found
}

/// True when `needle` stands as a keyword token anywhere in `haystack`.
pub(crate) fn contains_keyword_token(haystack: &str, needle: &str) -> bool {
    !keyword_token_positions(haystack, needle, IdentifierExemption::SigilOnly).is_empty()
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

/// True when the token starting at `start` sits where Cypher expects an
/// IDENTIFIER rather than a clause, so the keyword there names something
/// instead of doing something.
///
/// Two introducers:
///
/// * `.` or `:` — after `:` Cypher expects a label, a relationship type or a
///   map key, and after `.` a property name. Neither can introduce a clause.
///   Without this, `MATCH (i:Import)` tripped the `IMPORT` entry and every
///   query over this schema's `Import` node table was refused, including the
///   `MATCH (f:File)-[:Defines_File_Import]->(n:Import)` shape the accuracy
///   corpora in `benches/corpora/*/ground_truth.json` are written against
///   (fleet-watch review finding 1).
/// * `AS` — the next token is an alias. Measured on lbug 0.19.1:
///   `RETURN 1 AS limit` PARSES, so a reserved word is accepted as a bare
///   alias and `AS limit` is a reachable shape, not a hypothetical one
///   (pinned by `a_reserved_word_is_accepted_as_a_bare_alias`). Treating it as
///   a clause made `has_limit_clause` report a declared bound and suppress the
///   injection. Exempting it costs the gate nothing: whatever follows `AS` is
///   consumed as the alias, so a real clause after it is a separate token that
///   this scan still sees.
fn in_identifier_position(bytes: &[u8], start: usize, exemption: IdentifierExemption) -> bool {
    let mut j = start;
    // Only separators may be skipped. A MASKED_TOKEN is a value sitting in a
    // token position, so the walk STOPS on it — crossing it is what turned a
    // backticked alias into an invisible gap.
    while j > 0 && is_separator(bytes[j - 1]) {
        j -= 1;
    }
    if j == 0 {
        return false;
    }
    if matches!(bytes[j - 1], b'.' | b':') {
        return true;
    }
    if exemption == IdentifierExemption::SigilOnly {
        return false;
    }
    // `AS` as its own word. The haystack is case-folded by the caller, and the
    // two folds in this crate disagree (the gate upper-cases, the clause
    // detectors lower-case), so compare case-insensitively.
    j >= 2
        && bytes[j - 2].eq_ignore_ascii_case(&b'a')
        && bytes[j - 1].eq_ignore_ascii_case(&b's')
        && (j == 2 || !is_ident_char(bytes[j - 3]))
}

#[cfg(test)]
#[path = "read_only_gate_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "read_only_gate_masking_tests.rs"]
mod masking_tests;

#[cfg(test)]
#[path = "read_only_gate_differential_tests.rs"]
mod differential_tests;
