// security_gates::coverage — what the gates could NOT check, said out loud
// (issue #450).
//
// Two facts the report used to leave to the reader:
//   - the unsafe-code check (S2) decides no language: no parser records an
//     `is_unsafe` property (no node table has the column, graph_store/ddl.rs),
//     Rust included, so every changed symbol is skipped by it;
//   - a changed symbol that does not resolve in the graph gets none of the
//     five checks, which is not an informational detail.
//
// This module builds the unresolved-symbol flag and the report's
// `language_coverage` block from the flags, so the statement and the flags
// cannot disagree.

use super::SecurityFlag;
use crate::graph_store::{cypher_str, GraphStore};
use crate::prd_validator::unverifiable_reason;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// The languages the unsafe-code check decides. Empty on purpose: a language
/// enters this list with the parser change that records `is_unsafe` for it.
/// source: stages/stage-8.md §7 Q1; `grep is_unsafe src/parser src/graph_store`
/// finds no writer.
pub(super) const UNSAFE_CHECKED_LANGUAGES: &[&str] = &[];

/// The label a symbol's language takes in the report when its node carries
/// none (an older graph, or a node kind the indexer does not tag).
pub(super) const LANGUAGE_NOT_RECORDED: &str = "not_recorded";

/// The `language` property of the symbol node `qualified_name` names, read
/// label by label (lbug has no label alternation). `None` when no node of a
/// symbol label carries a non-empty language for it.
pub(super) fn symbol_language(store: &GraphStore, qualified_name: &str) -> Option<String> {
    let escaped = cypher_str(qualified_name);
    crate::clustering::SYMBOL_LABELS.iter().find_map(|label| {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = {escaped} RETURN n.language LIMIT 1"
        );
        let rows = store.execute_query(&cypher).ok()?;
        let language = rows.rows.into_iter().next()?.into_iter().next()?;
        let language = language.trim();
        (!language.is_empty()).then(|| language.to_string())
    })
}

/// The flag for a changed symbol that did not resolve in the graph. A
/// `warning`, not an `info`: no check ran on it, so its absence of findings
/// says nothing. When its file is outside the indexer's coverage (an
/// extension no parser handles, or no File node), the flag says so.
pub(super) fn unresolved_flag(
    store: &GraphStore,
    input: &str,
    did_you_mean: &[String],
) -> SecurityFlag {
    let reason = unverifiable_reason(store, input);
    let message = match &reason {
        Some(reason) => format!(
            "changed symbol '{input}' is outside the graph ({reason}): none of the \
             checks ran on it"
        ),
        None => format!(
            "changed symbol '{input}' did not resolve in graph: none of the checks \
             ran on it"
        ),
    };
    SecurityFlag {
        gate: "input_unresolved".into(),
        severity: "warning".into(),
        symbol: input.into(),
        message,
        details: json!({
            "did_you_mean": did_you_mean,
            "checks_run": 0,
            "unverifiable_reason": reason,
        }),
    }
}

/// The report's `language_coverage` block: per check that cannot decide every
/// language, the languages it decides, the languages of this call's changed
/// symbols it did not, and how many symbols that leaves unchecked. Derived
/// from the skip flags, so it counts exactly what was skipped.
pub(super) fn language_coverage(flags: &[SecurityFlag]) -> Value {
    let skipped: Vec<&SecurityFlag> = flags
        .iter()
        .filter(|f| f.gate == "unsafe_symbol" && f.details["skipped"] == true)
        .collect();
    let not_checked: BTreeSet<&str> = skipped
        .iter()
        .map(|f| {
            f.details["language"]
                .as_str()
                .unwrap_or(LANGUAGE_NOT_RECORDED)
        })
        .collect();
    json!({
        "unsafe_symbol": {
            "checked_languages": UNSAFE_CHECKED_LANGUAGES,
            "not_checked_languages": not_checked,
            "symbols_not_checked": skipped.len(),
            "note": "The unsafe-code check decides no language yet: no parser \
                     records `is_unsafe`, Rust included. `not_checked_languages` \
                     lists the languages of this call's changed symbols; a \
                     report without an unsafe flag is not evidence that no \
                     changed symbol is unsafe.",
        },
    })
}
