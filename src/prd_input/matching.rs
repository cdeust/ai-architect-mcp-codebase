// prd_input::matching — token extraction, graph search, and match-mode
// classification for Stage 4 (prepare_prd_input).
//
// ROOT CAUSE (issue #14): the original implementation ran every
// natural-language word from the finding/feature description through
// `search::search_graph` with `min_score: 0.0` and folded every hit into
// `matched_symbols` alongside a bundle-level `verified: true` flag. A common
// English word that happens to be a SUBSTRING of an unrelated symbol name
// (e.g. "anchor" inside `_CONCRETE_ANCHOR`) scores indistinguishably from a
// genuine partial-word hit on the real target (e.g. "handle" inside
// `handle_tool_call`) — see `test_lexical_and_exact_score_identically_by_ratio`
// below, measured on this repo's own graph. A score threshold therefore
// CANNOT separate true positives from false positives; the score alone does
// not carry that information (Move 4 classification: (a) missing/wrong
// contract — the function accepted "search returned a hit" as evidence of
// relatedness when it isn't).
//
// FIX: classify every hit by its MATCH MODE, not its raw score:
//   - Verbatim  — the description cited the identifier in backticks AND the
//     graph has a symbol whose name or qualified-name tail equals it exactly.
//     Prioritized per issue #14 point 3.
//   - ExactName — a natural-language token equals a symbol's name or
//     qualified-name tail exactly (not merely contains it).
//   - Lexical   — anything else: the token is a substring/fuzzy hit only.
// Verbatim and ExactName hits are structural evidence of relatedness (the
// user or the codebase itself asserted the identifier) and are surfaced as
// `matched_symbols` (trustworthy grounding). Lexical hits carry no such
// evidence — they are surfaced separately as `candidate_symbols`, tagged
// with their raw score, and NEVER folded into `matched_symbols` or into
// impacted_communities/impacted_processes. An empty `matched_symbols` is the
// correct output when only lexical hits exist (issue #14: "a misleading
// bundle is worse than an empty one").
//
// source: stage-4 brief (token-per-word search cap); issue #14 root-cause
// discussion (this file's header); RRF citation retained in `crate::search`.

use crate::graph_store::{cypher_str, GraphStore, NODE_FILE};
use crate::search;
use serde_json::{json, Value};
use std::path::Path;

// Minimum token length — single-letter tokens ("a", "i") produce noise.
// source: Lucene StandardAnalyzer default behavior (min length 2+ for
// general-purpose text indexing). Stricter than BM25's bare minimum so the
// per-token search stays focused on symbol-like words.
pub const MIN_TOKEN_LEN: usize = 3;

// Cap token count so a pathologically-long description can't explode the
// search budget. Applied independently to natural-language tokens and to
// backtick-cited identifiers. source: stage-4 brief ("compact artifact") —
// no paper source.
pub const MAX_TOKENS: usize = 32;

// source: stage-4 brief — "top-3 matches per token" caps the cost of
// description-driven search. Not paper-backed; chosen to keep output
// compact while capturing the obvious per-token hit plus two backups.
pub const MATCHES_PER_TOKEN: usize = 3;

/// How a symbol earned its place in the output. Drives inclusion in
/// `matched_symbols` (Verbatim/ExactName) vs `candidate_symbols` (Lexical).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MatchMode {
    /// Description cited the identifier verbatim in backticks, and it
    /// resolved to an exact symbol name / qualified-name tail.
    Verbatim,
    /// A natural-language token equals the symbol's name (or qualified-name
    /// tail) exactly — case-insensitive, but not merely a substring.
    ExactName,
    /// Substring/fuzzy hit only — no exact-identity evidence.
    Lexical,
}

impl MatchMode {
    fn as_str(self) -> &'static str {
        match self {
            MatchMode::Verbatim => "verbatim",
            MatchMode::ExactName => "exact_name",
            MatchMode::Lexical => "lexical",
        }
    }

    /// Trust ordering: Verbatim > ExactName > Lexical. Used by
    /// `search_and_classify`/`upsert_best` to make sure a symbol hit by
    /// several tokens with different modes always keeps its BEST evidence,
    /// regardless of which token was processed first.
    fn rank(self) -> u8 {
        match self {
            MatchMode::Lexical => 0,
            MatchMode::ExactName => 1,
            MatchMode::Verbatim => 2,
        }
    }
}

pub struct MatchedSymbol {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub community_id: Option<String>,
    pub community_size: Option<u64>,
    pub processes: Vec<String>,
    pub calls: Vec<String>,
    pub called_by: Vec<String>,
    pub uses: Vec<String>,
    pub match_mode: MatchMode,
    /// Raw score from `search::search_graph` (RRF-fused or substring-fallback
    /// scale — the two are not on a common calibrated scale, which is
    /// exactly why `match_mode`, not this score, decides trust). Exposed so
    /// consumers can rank candidates without re-querying the graph.
    pub confidence: f64,
}

pub struct MatchOutcome {
    /// Verbatim + ExactName hits — trustworthy grounding.
    pub matched: Vec<MatchedSymbol>,
    /// Lexical-only hits — exposed for visibility, never treated as verified.
    pub candidates: Vec<MatchedSymbol>,
}

/// Extracts identifiers the description cited verbatim in backticks, e.g.
/// "the `grad_rgb` helper" → ["grad_rgb"]. Order-preserving, deduplicated,
/// capped at MAX_TOKENS. Punctuation adjacent to the identifier inside the
/// backticks (call parens, trailing commas) is stripped by `clean_token`.
pub fn extract_verbatim_identifiers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('`') else {
            break;
        };
        let raw = &after_open[..close];
        let cleaned = clean_token(raw);
        if !cleaned.is_empty() && !out.iter().any(|t| t == &cleaned) {
            out.push(cleaned);
        }
        rest = &after_open[close + 1..];
        if out.len() >= MAX_TOKENS {
            break;
        }
    }
    out
}

/// Whitespace-split, lowercased, sanitized, deduplicated natural-language
/// tokens from a title+description pair. Tokens already captured as verbatim
/// identifiers are kept here too (dedup against verbatim happens at the
/// classification stage, not here) — this function stays a pure tokenizer.
pub fn tokenize_natural(title: &str, description: &str) -> Vec<String> {
    let combined = format!("{title} {description}");
    let mut seen: Vec<String> = Vec::new();
    for raw in combined.split_whitespace() {
        if seen.len() >= MAX_TOKENS {
            break;
        }
        let cleaned = clean_token(raw);
        if cleaned.len() < MIN_TOKEN_LEN {
            continue;
        }
        if !seen.iter().any(|t| t == &cleaned) {
            seen.push(cleaned);
        }
    }
    seen
}

pub fn clean_token(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Runs the search+classify pipeline: verbatim identifiers first (priority
/// per issue #14 point 3), then natural-language tokens.
///
/// A symbol can be hit by MULTIPLE tokens with DIFFERENT match modes — e.g.
/// the weak token "handle" (lexical, substring) and the exact token
/// "handle_tool_call" both resolve to the same qualified_name. Classifying
/// greedily on first-seen would let whichever token happens to be tokenized
/// first lock in the symbol's fate, silently downgrading a genuine exact
/// match to "already claimed as lexical" — this was a regression caught by
/// `tests::test_repro_issue14_lexical_false_positive_excluded_from_matched`'s
/// sibling `stage4_integration.rs` end-to-end test. Fix: accumulate the BEST
/// mode seen per qualified_name across every token before classifying, using
/// `MatchMode::rank` (Verbatim > ExactName > Lexical) so a later, stronger
/// hit always upgrades an earlier, weaker one and a later weaker hit never
/// downgrades an already-confirmed one.
/// `index_dir` is the resolved search-index directory for the graph being
/// searched (see `search::resolve_search_index_dir`), or `None` when no
/// hybrid BM25/vector index was built for it — `search_hits` then falls
/// back to `search::search_graph`'s substring scorer, same as before issue
/// #18. Passing `None` here is a legitimate, explicit choice by the caller
/// (e.g. no index yet); it must never happen SILENTLY by construction the
/// way it did pre-fix, where this function always passed `None` regardless
/// of what the caller had available (see `mod.rs::prepare`'s explicit
/// `eprintln!` when it resolves no index).
pub fn search_and_classify(
    store: &GraphStore,
    verbatim_tokens: &[String],
    natural_tokens: &[String],
    index_dir: Option<&Path>,
) -> MatchOutcome {
    let mut best: Vec<(search::SearchResult, MatchMode)> = Vec::new();

    for token in verbatim_tokens {
        for hit in search_hits(store, token, index_dir) {
            if !is_exact_hit(token, &hit) {
                continue; // unverifiable citation — drop, not a candidate
            }
            upsert_best(&mut best, hit, MatchMode::Verbatim);
        }
    }

    for token in natural_tokens {
        for hit in search_hits(store, token, index_dir) {
            let mode = if is_exact_hit(token, &hit) {
                MatchMode::ExactName
            } else {
                MatchMode::Lexical
            };
            upsert_best(&mut best, hit, mode);
        }
    }

    let mut matched: Vec<MatchedSymbol> = Vec::new();
    let mut candidates: Vec<MatchedSymbol> = Vec::new();
    for (hit, mode) in best {
        let enriched = enrich(store, hit, mode);
        if mode == MatchMode::Lexical {
            candidates.push(enriched);
        } else {
            matched.push(enriched);
        }
    }

    MatchOutcome {
        matched,
        candidates,
    }
}

/// Inserts `hit` into `best` keyed by qualified_name, keeping the
/// highest-ranked `MatchMode` seen so far for that symbol (see
/// `search_and_classify` doc comment for why this must never downgrade).
fn upsert_best(
    best: &mut Vec<(search::SearchResult, MatchMode)>,
    hit: search::SearchResult,
    mode: MatchMode,
) {
    if let Some(existing) = best
        .iter_mut()
        .find(|(h, _)| h.qualified_name == hit.qualified_name)
    {
        if mode.rank() > existing.1.rank() {
            existing.1 = mode;
        }
        return;
    }
    best.push((hit, mode));
}

/// Search hits for one token, restricted to CODE SYMBOLS.
///
/// `label_filter` stays `None` — the filter would have to name a single kind,
/// and every symbol kind is wanted here — so the exclusion is applied to the
/// results instead (review round 3, finding 3). Since fleet-watch#112,
/// `search_graph` can also return `File`-labeled doc-content hits: a markdown
/// or plain-text file surfaced by its PROSE. Those are not symbols, and
/// everything downstream of this function treats what it returns as verified
/// code-symbol grounding for PRD generation — `matched_symbols` is documented
/// as exactly that. A doc file reaching it would be presented to a PRD author
/// as a code symbol the graph confirms exists.
///
/// Severity, stated precisely rather than dramatised: no doc file can reach
/// `matched` on today's code, because `is_exact_hit` compares against tokens
/// that `clean_token` has stripped of `.` (its own test pins
/// `clean_token("bar.baz") == "barbaz"`), while every doc hit's `name` is a
/// filename that necessarily carries one — a `File` only becomes a BM25 doc
/// document by having a `DOC_EXTENSIONS` extension. So a doc hit always
/// classifies `Lexical` and lands in `candidates`. That is an accident of two
/// unrelated rules meeting, not a decision: nothing states it, no test pins it,
/// and either rule may change. The label check makes the guarantee explicit and
/// stops it resting on that coincidence.
fn search_hits(
    store: &GraphStore,
    token: &str,
    index_dir: Option<&Path>,
) -> Vec<search::SearchResult> {
    let opts = search::SearchOptions {
        limit: MATCHES_PER_TOKEN,
        label_filter: None,
        min_score: 0.0,
    };
    search::search_graph(store, token, &opts, index_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(is_code_symbol)
        .collect()
}

/// True for a hit the graph holds as a code symbol, false for a doc-content
/// `File` hit. The PRD matcher grounds claims about CODE; a file surfaced by
/// its prose is evidence of documentation, not of a symbol.
fn is_code_symbol(hit: &search::SearchResult) -> bool {
    !hit.label.eq_ignore_ascii_case(NODE_FILE)
}

/// True iff `token` equals the hit's name or the tail of its qualified name
/// (both case-insensitive; `token` is already lowercased by `clean_token`).
/// This is INDEPENDENT of the hit's score — the substring-fallback and
/// hybrid-RRF search paths score on different, uncalibrated scales, so score
/// alone cannot distinguish exact identity from partial overlap. String
/// equality is the only signal that transfers across both paths.
///
/// Known limit (not fixed by issue #14, documented in
/// `test_homonymous_symbols_in_different_files_both_classify_as_exact`):
/// this compares by name only, with no file/module disambiguation. Two
/// distinct symbols in different files sharing a name both classify as
/// exact for the same token — there is no signal in a free-text description
/// to tell them apart.
fn is_exact_hit(token: &str, hit: &search::SearchResult) -> bool {
    if hit.name.to_ascii_lowercase() == token {
        return true;
    }
    let qn_tail = hit
        .qualified_name
        .rsplit("::")
        .next()
        .unwrap_or(&hit.qualified_name);
    qn_tail.to_ascii_lowercase() == token
}

fn enrich(store: &GraphStore, hit: search::SearchResult, match_mode: MatchMode) -> MatchedSymbol {
    let confidence = hit.score;
    let community_size = hit
        .community_id
        .as_ref()
        .and_then(|cid| lookup_community_size(store, cid));
    // 1-hop neighbors via get_context — reuses the SAME three-layer lookup
    // that get_context uses, so PRD consumers see exactly what the search
    // tools expose. Graceful degradation if the symbol is fuzzy-matched only.
    let (calls, called_by, uses) = match search::get_context(store, &hit.qualified_name) {
        Ok(ctx) => (
            ctx.calls.iter().map(|r| r.name.clone()).collect(),
            ctx.called_by.iter().map(|r| r.name.clone()).collect(),
            ctx.uses.iter().map(|r| r.name.clone()).collect(),
        ),
        Err(_) => (Vec::new(), Vec::new(), Vec::new()),
    };
    MatchedSymbol {
        qualified_name: hit.qualified_name,
        name: hit.name,
        label: hit.label,
        file_path: hit.file_path,
        community_id: hit.community_id,
        community_size,
        processes: hit.process_names,
        calls,
        called_by,
        uses,
        match_mode,
        confidence,
    }
}

fn lookup_community_size(store: &GraphStore, community_id: &str) -> Option<u64> {
    let escaped = cypher_str(community_id);
    let cypher =
        format!("MATCH (c:Community) WHERE c.id = {escaped} RETURN c.member_count LIMIT 1");
    let qr = store.execute_query(&cypher).ok()?;
    let row = qr.rows.first()?;
    row.first().and_then(|s| s.parse::<u64>().ok())
}

pub fn matched_to_json(m: &MatchedSymbol) -> Value {
    json!({
        "qualified_name": m.qualified_name,
        "name": m.name,
        "label": m.label,
        "file_path": m.file_path,
        "community_id": m.community_id,
        "community_size": m.community_size,
        "processes": m.processes,
        "match_mode": m.match_mode.as_str(),
        "confidence": m.confidence,
        "relationships": {
            "calls": m.calls,
            "called_by": m.called_by,
            "uses": m.uses,
        }
    })
}

pub fn dedup_keys<F>(matched: &[MatchedSymbol], key: F) -> Vec<String>
where
    F: Fn(&MatchedSymbol) -> Option<String>,
{
    let mut out: Vec<String> = Vec::new();
    for m in matched {
        if let Some(k) = key(m) {
            if !k.is_empty() && !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

pub fn impacted_processes_from_symbols(matched: &[MatchedSymbol]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in matched {
        for p in &m.processes {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests — pure helpers that don't need a graph, plus the measured evidence
// that a score threshold cannot separate lexical false positives from
// genuine partial-word hits (the basis for the match_mode design above).
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "matching_tests.rs"]
mod tests;
