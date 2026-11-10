// prd_validator::verdict — the claim-verdict cluster from issue #13's fix:
// classifying a resolved-or-not claim into Resolved / Hallucinated /
// Unverifiable / Absent (issue #450) / Unscored, and the graph lookups it
// depends on.
//
// Split out of prd_validator::mod so the "is this claim real, hallucinated,
// or simply unverifiable" question lives in one small, reviewable place
// (coding-standards §4.1/§4.2 — prd_validator.rs was 929 lines before this
// split). Shared by `axis_symbol` (finding severity) and `mod::validate_prd`
// (summary counters) so the two never diverge on what counts as
// "hallucinated".

use super::{ClaimOrigin, ResolvedClaim};
use crate::graph_store::{cypher_str, GraphStore, CFG_GATE_LABELS};
use crate::language_provider::ALL_EXTENSIONS;
use crate::search::strip_leading_path_component;
use std::path::Path;

// `Unscored` covers claims outside the structured-modify gate (change_kind
// `add`/`unknown`, or regex-fallback tokens that are not `Absent`) —
// unchanged pre-existing behavior: they were never critical findings and are
// not counted as hallucinated in the summary either (see issue #13
// discussion; regex fallback gets its own "info" finding via
// `emit_unresolved_info`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClaimVerdict {
    /// resolved_qn was Some — not a candidate for this axis at all.
    Resolved,
    /// Symbol absent from a file the indexer actually covers: a real claim
    /// the graph can refute. Critical finding.
    Hallucinated,
    /// Symbol absent, but its file is outside indexer coverage (unsupported
    /// language extension, or no File node at all despite an indexed-looking
    /// extension — pruned, filtered, or never walked). The graph cannot
    /// confirm or refute the claim. Info finding, not counted as hallucinated.
    Unverifiable(String),
    /// Regex fallback only (issue #450): a backticked token that resolves to
    /// nothing AND whose leaf names no code symbol of any kind (Function,
    /// Method, Struct, Enum, Variant, Trait, Field, Module, Constant,
    /// TypeAlias — `CFG_GATE_LABELS`) — the shape of a fabricated class. Without the stage-5 contract its
    /// change_kind is unknown (it may be a symbol the PRD adds, or an
    /// external name), so it is a warning, not a critical hallucination.
    /// A token whose name does exist (only its qualification failed) stays
    /// `Unscored`: the graph shows the PRD is talking about something real.
    /// So does a token whose leaf is not an identifier, or whose existence
    /// probe failed: neither proves anything is missing.
    Absent,
    /// Outside the structured-modify gate — not evaluated by this axis.
    Unscored,
}

pub(super) fn classify_unresolved(
    store: &GraphStore,
    resolved: &[ResolvedClaim],
) -> Vec<ClaimVerdict> {
    resolved.iter().map(|r| classify_one(store, r)).collect()
}

fn classify_one(store: &GraphStore, r: &ResolvedClaim) -> ClaimVerdict {
    if r.resolved_qn.is_some() {
        return ClaimVerdict::Resolved;
    }
    if r.claim.origin == ClaimOrigin::Backticked {
        // A bare or `A::B` token carries no file path, so the file coverage
        // check below cannot apply to it: the only question is whether any
        // code symbol bears its leaf name. Only a definite "no" is `Absent`;
        // a failed probe (`None`) says nothing and stays `Unscored`.
        let leaf = r.claim.token.rsplit("::").next().unwrap_or(&r.claim.token);
        // The extractor accepts any `[A-Za-z0-9_:]` run, so `Tool::` (empty
        // leaf) or `localhost:8080` (a single `:`) reach here. Neither is a
        // symbol name, so neither can be judged absent (PR #487 review).
        // A non-empty `did_you_mean` already proves the leaf names a symbol;
        // the probe below could only say yes.
        if !is_identifier(leaf) || !r.did_you_mean.is_empty() {
            return ClaimVerdict::Unscored;
        }
        return match name_exists(store, leaf) {
            Some(false) => ClaimVerdict::Absent,
            Some(true) | None => ClaimVerdict::Unscored,
        };
    }
    let kind = r.claim.change_kind.as_str();
    if !matches!(kind, "modify" | "remove" | "rename") {
        return ClaimVerdict::Unscored;
    }
    match unverifiable_reason(store, &r.claim.token) {
        Some(reason) => ClaimVerdict::Unverifiable(reason),
        None => ClaimVerdict::Hallucinated,
    }
}

// A claimed qualified_name follows `<file_path>::name[::name...]` — file_path
// up to the FIRST `::`, with the rest naming a possibly-nested symbol
// (method, nested module). source: tests/corpus_full.rs::file_id_of, whose
// doc comment cites the authoritative convention from parser/mod.rs::qual;
// the same first-`::` split search::resolve_qualified_name's layer-2
// strip-prefix retry relies on. Splitting on the LAST `::` instead would
// mis-extract the file_path of any multi-segment QN (e.g.
// `src/foo.rs::MyStruct::new` -> wrongly "src/foo.rs::MyStruct"), routing
// real hallucinations into Unverifiable. Returns None when the token
// carries no file prefix at all (a bare identifier) — those cannot be
// checked against file coverage and are conservatively treated as
// verifiable-and-absent (Hallucinated), preserving pre-existing behavior
// for tokens without path context.
fn claim_file_path(token: &str) -> Option<&str> {
    let idx = token.find("::")?;
    let file_path = &token[..idx];
    if file_path.contains('.') {
        Some(file_path)
    } else {
        None
    }
}

// source: issue #13 — a claim's file may be unverifiable for two distinct
// reasons, both meaning "the indexer never produced a File node here":
//   1. The extension isn't in language_provider::ALL_EXTENSIONS at all — the
//      indexer has no parser for it (e.g. bash `.sh`).
//   2. The extension IS supported, but no File node exists for this path —
//      the file was pruned (dependency scope), excluded by a language
//      filter, or simply never walked. Same observable effect: unverifiable.
// Distinguished only in the message text; both return Some(reason).
pub(crate) fn unverifiable_reason(store: &GraphStore, token: &str) -> Option<String> {
    let file_path = claim_file_path(token)?;
    let ext = Path::new(file_path).extension().and_then(|e| e.to_str());
    if let Some(ext) = ext {
        if !ALL_EXTENSIONS.contains(&ext) {
            return Some(format!(
                "language not indexed: '.{ext}' has no parser (file '{file_path}')"
            ));
        }
    }
    // Check graph coverage against both the raw claimed path and the
    // parser's stripped-leading-path-component form (the same retry
    // search::resolve_qualified_name performs at layer 2 — the indexer
    // strips a leading `src/`-style component when building
    // qualified_names, so a claim like `src/main.rs::foo` must be checked
    // against `main.rs` too before concluding the file is unindexed).
    if file_has_graph_node(store, file_path) {
        return None;
    }
    if let Some(stripped_token) = strip_leading_path_component(token) {
        if let Some(stripped_file_path) = claim_file_path(&stripped_token) {
            if file_has_graph_node(store, stripped_file_path) {
                return None;
            }
        }
    }
    Some(format!(
        "file not present in the indexed graph: '{file_path}' \
         (outside indexed scope, or excluded by a language/dependency filter)"
    ))
}

// A symbol name as every indexed language spells one: a letter or `_`, then
// letters, digits or `_`.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// Whether any code symbol is named `leaf`: `Some(true)`/`Some(false)` when
// every probe ran, `None` when one failed. Deliberately NOT
// search::name_lookup's `did_you_mean` list: that one covers only
// SEARCHABLE_LABELS and drops query errors, so a backticked enum variant or
// struct field (Variant/Field nodes) would read as absent, and so would any
// name whose lookup errored (PR #487 review). CFG_GATE_LABELS is the set of
// labels that are named code symbols; every one has a `name` column, which
// verdict_tests::test_name_exists_probes_every_label_and_reports_failure
// pins (a label without one would make every probe fail, i.e. `None`).
fn name_exists(store: &GraphStore, leaf: &str) -> Option<bool> {
    let literal = cypher_str(leaf);
    for label in CFG_GATE_LABELS {
        let cypher = format!("MATCH (n:{label}) WHERE n.name = {literal} RETURN n.id LIMIT 1");
        if !store.execute_query(&cypher).ok()?.rows.is_empty() {
            return Some(true);
        }
    }
    Some(false)
}

fn file_has_graph_node(store: &GraphStore, file_path: &str) -> bool {
    // source: M1 fix precedent in git_diff.rs — manual `.replace('\'', ...)`
    // escaping is vulnerable to a `\'` payload that closes the string early;
    // claim.token originates from LLM-generated PRD content and must go
    // through the tested cypher_str helper, not ad-hoc escaping.
    let literal = cypher_str(file_path);
    let cypher = format!("MATCH (f:File) WHERE f.path = {literal} RETURN f.id LIMIT 1");
    matches!(store.execute_query(&cypher), Ok(qr) if !qr.rows.is_empty())
}

// ---------------------------------------------------------------------------
// Tests — pure helpers plus the adversarial-input regression for
// file_has_graph_node.
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "verdict_tests.rs"]
mod tests;
