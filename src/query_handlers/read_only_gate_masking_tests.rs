//! What the MASKED view means, and what may walk across it.
//!
//! Split from `read_only_gate_tests` when that file crossed the §4.1 cap, along
//! the seam the re-review exposed: the sibling file asks "is this keyword
//! refused?", while everything here asks the prior question — "what was there
//! before masking, and is it a separator or a token?". Three confirmed defects
//! came from answering it wrong, so the two concerns are worth reading apart.

use super::*;

// ---------------------------------------------------------------------------
// The masked view is CONTENT, not whitespace
//
// Re-review findings 1, 2 and 4 shared one root cause: a look-back or a trim
// walked across masked bytes as if nothing had been there. Comments and
// literals were both blanked to spaces, though a comment IS a separator and a
// literal is a TOKEN. The fixture family below therefore always covers the
// three shapes that hid it: ends-with-literal, ends-with-backtick, and a mask
// sitting exactly at the edge of a look-back window.
// ---------------------------------------------------------------------------

/// SECURITY. A backticked alias masked to spaces let the `AS` look-back walk
/// straight over it, so the keyword after it was read as an alias and exempted.
/// Both shapes below reached the engine before this fix.
#[test]
fn a_backticked_alias_does_not_shield_the_following_keyword() {
    assert!(
        forbidden_cypher_keyword("WITH n AS `m` DELETE m").is_some(),
        "DELETE after a backticked alias must still be refused"
    );
    assert!(
        forbidden_cypher_keyword("WITH 1 AS `x` CALL storage_info() RETURN *").is_some(),
        "CALL after a backticked alias must still face the procedure allowlist"
    );
    // A string literal in the same position behaves identically.
    assert!(forbidden_cypher_keyword("WITH n AS 'm' DELETE m").is_some());
}

/// The gate carries NO alias exemption at all: on this scan an exemption can
/// only ever let a keyword through, so it fails closed even without backticks.
#[test]
fn the_gate_never_exempts_a_keyword_after_as() {
    assert!(forbidden_cypher_keyword("WITH 1 AS x CALL storage_info() RETURN *").is_some());
    assert!(forbidden_cypher_keyword("RETURN 1 AS create").is_some());
    // The clause detectors DO exempt it — that is the asymmetry, and it is
    // deliberate: there, a false positive suppresses the LIMIT injection.
    assert!(!crate::query_handlers::has_limit_clause(
        "RETURN 1 AS limit"
    ));
}

/// A comment IS a separator and must stay one, or the alias exemption and the
/// ORDER/BY pairing both break on legitimate queries.
#[test]
fn a_comment_still_reads_as_a_separator() {
    assert!(!crate::query_handlers::has_limit_clause(
        "RETURN 1 AS /* alias */ limit"
    ));
    assert!(crate::query_handlers::has_order_by_clause(
        "MATCH (n) RETURN n ORDER /* c */ BY n.id"
    ));
}

/// The mask sits exactly at the left edge of the look-back window.
#[test]
fn a_masked_token_at_the_lookback_edge_stops_the_walk() {
    // Nothing but the masked token precedes the keyword.
    assert!(forbidden_cypher_keyword("`a` DELETE n").is_some());
    assert!(forbidden_cypher_keyword("'a' DELETE n").is_some());
    // …and at index 0 there is no look-back to do.
    assert!(forbidden_cypher_keyword("DELETE n").is_some());
}
