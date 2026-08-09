// parser::spec::structural_derive_pairing — the DEFINITION+CALL half of
// issue #232's rule auto-derivation, split out of `structural_derive.rs`
// purely for the coding-standards.md §4.1 500-line file cap. See that
// module's doc for the shared vocabulary (corpus parsing, candidate
// enumeration, text extraction) this module builds on.
//
// ## Why definitions and calls are derived JOINTLY, not independently
// An early version of this module scored definitions in isolation — "is
// this name referenced anywhere else as a token?" — and calls in isolation
// against whatever definition rule won first. That failed the control test
// (`structural_derive_tests`): in Go's own corpus, `short_var_declaration`'s
// `left` field (an ordinary local-variable assignment target) scored a
// perfect 1.0 on "referenced elsewhere", same as `function_declaration`'s
// `name` field — because EVERY local variable is, trivially, referenced
// again after it's declared. Token-recurrence alone cannot separate "this
// is a reusable named THING other code points at" from "this is an ordinary
// binding used in its own enclosing scope." What DOES separate them is
// exactly the issue's own two-sided definition: a real definition rule and
// a real call rule are mutually consistent with EACH OTHER, at a real
// resolution rate. `derive_definition_and_call_rules` therefore scores
// every (definition-candidate, call-candidate) PAIR by both directions at
// once (`PairCandidate::def_rate`/`call_rate`) and keeps the pair that
// clears `RESOLUTION_THRESHOLD` on BOTH sides with the highest combined
// (worst-of-the-two) score — never a candidate validated in isolation.
//
// Two further exclusions were needed once pairing existed, both verified
// necessary in this PR's own control run — see `extracted_target_kinds`'s
// and `is_call_shaped_kind`'s own docs for the specific failures each one
// closes.

use std::collections::HashSet;

use tree_sitter::Node;

use super::structural_derive::{
    extracted_texts, is_identifier_like, last_segment, observed_candidates, walk_all, Candidate,
    CorpusFile, Source, DEFINITION_DISTINCTNESS_THRESHOLD, RESOLUTION_THRESHOLD,
};
use std::collections::HashMap;

/// One scored (definition-candidate, call-candidate) PAIR — see the module
/// doc's "why jointly" section for why neither side is scored alone.
#[derive(Clone, Debug)]
pub(crate) struct PairCandidate {
    pub def: Candidate,
    pub call: Candidate,
    pub def_matched: usize,
    pub def_total: usize,
    pub def_rate: f64,
    pub call_matched: usize,
    pub call_total: usize,
    pub call_rate: f64,
}

impl PairCandidate {
    fn min_rate(&self) -> f64 {
        self.def_rate.min(self.call_rate)
    }
}

#[derive(Debug)]
pub(crate) enum PairOutcome {
    Resolved(PairCandidate),
    Unresolved {
        pairs_tried: usize,
        best_min_rate: f64,
    },
}

/// The node KIND(s) a candidate's source resolves TO, across every
/// instance — e.g. Go's `short_var_declaration.left` field always resolves
/// to an `expression_list` node. Used to detect and exclude FIELD ALIASING:
/// a def candidate and a call candidate that are, structurally, the SAME
/// underlying node reached two different ways (a parent's field vs. that
/// exact child's own kind) must never be paired — matching there is not
/// resolution, it is a field pointing at itself. Verified necessary in this
/// PR's own control run: `short_var_declaration` "won" over the real
/// `function_declaration` definition purely from this self-pairing before
/// this exclusion existed.
fn extracted_target_kinds(files: &[CorpusFile], candidate: &Candidate) -> HashSet<String> {
    let mut kinds = HashSet::new();
    for file in files {
        for node in walk_all(file.tree.root_node()) {
            if node.kind() != candidate.node_kind {
                continue;
            }
            let target = match &candidate.source {
                Source::Field(name) => node.child_by_field_name(name),
                Source::FirstNamedChild => node.named_child(0),
            };
            if let Some(target) = target {
                kinds.insert(target.kind().to_string());
            }
        }
    }
    kinds
}

/// Field/child names that signal "this node carries an argument list" —
/// the SAME generic vocabulary `structural.rs::call_callee_field` and
/// `structural_fallback.rs`'s fieldless-call tier already use and already
/// validated (issue #224) across Go/Java/Python/Rust/TypeScript/C-family/
/// Ruby/Lua/Bash/Elixir/Zig's real grammars — reused here, not reinvented,
/// as a CALL-role eligibility gate: a genuine call site carries a callee
/// AND an argument list; a plain `name=value` binding (Python's
/// `default_parameter`, `keyword_argument`) carries only a name and a
/// value, never an argument list. Verified necessary in this PR's own
/// control run: Python's `default_parameter`/`keyword_argument` out-scored
/// the real `function_definition`/`call` pair before this gate existed —
/// Python code commonly calls with keyword arguments matching parameter
/// names, so the coincidence was real, just not a call construct.
const ARGUMENT_LIST_FIELD_CANDIDATES: [&str; 2] = ["arguments", "argument"];

fn has_argument_list_structure(node: Node) -> bool {
    if ARGUMENT_LIST_FIELD_CANDIDATES
        .iter()
        .any(|f| node.child_by_field_name(f).is_some())
    {
        return true;
    }
    let mut cursor = node.walk();
    let has_arguments_child = node
        .named_children(&mut cursor)
        .any(|c| c.kind() == "arguments");
    has_arguments_child
}

/// Whether MOST instances of `node_kind` carry an argument-list structure
/// (`has_argument_list_structure`) — computed once per node kind (not per
/// candidate/source) since it is a property of the KIND, not of which field
/// this particular candidate reads.
fn is_call_shaped_kind(files: &[CorpusFile], node_kind: &str) -> bool {
    let mut total = 0usize;
    let mut shaped = 0usize;
    for file in files {
        for node in walk_all(file.tree.root_node()) {
            if node.kind() != node_kind {
                continue;
            }
            total += 1;
            if has_argument_list_structure(node) {
                shaped += 1;
            }
        }
    }
    total > 0 && (shaped as f64 / total as f64) >= 0.5
}

/// A precomputed candidate: its raw extracted texts (one per corpus
/// instance) plus, when every one of them is identifier-like, its
/// definition-eligibility (`def_names`/`distinctness`) — computed ONCE per
/// candidate so the O(candidates^2) pairing loop in
/// `derive_definition_and_call_rules` never re-walks the corpus.
struct CandidateProfile {
    candidate: Candidate,
    texts: Vec<String>,
    /// `Some(names)` when EVERY extracted text is identifier-like (a
    /// definition's name must be a plain identifier, never a dotted
    /// reference) and the distinctness gate clears
    /// `DEFINITION_DISTINCTNESS_THRESHOLD`.
    def_names: Option<Vec<String>>,
    /// See `extracted_target_kinds` — the node kind(s) this candidate's
    /// source resolves to, used to exclude field-aliased pairs.
    target_kinds: HashSet<String>,
    /// See `is_call_shaped_kind` — whether this candidate's node kind
    /// itself looks like a call site (callee text PLUS an argument list),
    /// required for CALL-role eligibility only.
    call_shaped: bool,
}

fn build_profiles(files: &[CorpusFile]) -> Vec<CandidateProfile> {
    let mut call_shaped_cache: HashMap<String, bool> = HashMap::new();
    observed_candidates(files)
        .into_iter()
        .filter_map(|candidate| {
            let texts = extracted_texts(files, &candidate);
            if texts.is_empty() {
                return None;
            }
            let all_identifier = texts.iter().all(|t| is_identifier_like(t));
            let def_names = if all_identifier {
                let distinct: HashSet<&str> = texts.iter().map(String::as_str).collect();
                let distinctness = distinct.len() as f64 / texts.len() as f64;
                if distinctness >= DEFINITION_DISTINCTNESS_THRESHOLD {
                    Some(texts.clone())
                } else {
                    None
                }
            } else {
                None
            };
            let target_kinds = extracted_target_kinds(files, &candidate);
            let call_shaped = *call_shaped_cache
                .entry(candidate.node_kind.clone())
                .or_insert_with(|| is_call_shaped_kind(files, &candidate.node_kind));
            Some(CandidateProfile {
                candidate,
                texts,
                def_names,
                target_kinds,
                call_shaped,
            })
        })
        .collect()
}

/// A pair needs at least this many instances on EACH side before its rate
/// is trusted at all — a 1/1 coincidental match (verified to occur in this
/// PR's own SQL run: `function_argument`/`column_definition` scored a
/// "perfect" 1.0/1.0 on a SINGLE instance each) is not evidence, it's noise
/// with a suspiciously round ratio. Source: measured in this PR's own
/// control run — the same run that motivated `DEFINITION_DISTINCTNESS_THRESHOLD`.
const MIN_PAIR_SUPPORT: usize = 3;

/// Derives the DEFINITION and CALL rules JOINTLY — see the module doc's
/// "why jointly" section. Every definition-eligible candidate is paired
/// against every OTHER candidate of a DIFFERENT node kind (same-kind pairs
/// are excluded structurally, not just deprioritized — two candidates that
/// share a node kind are observing the SAME underlying node instances under
/// two different extraction sources). The pair clearing `RESOLUTION_THRESHOLD`
/// on both sides, with `MIN_PAIR_SUPPORT` instances on each, and the
/// highest worst-of-the-two rate wins.
pub(crate) fn derive_definition_and_call_rules(
    files: &[CorpusFile],
) -> (PairOutcome, Vec<PairCandidate>) {
    let profiles = build_profiles(files);
    let mut pairs: Vec<PairCandidate> = Vec::new();

    for def_profile in &profiles {
        if def_profile.def_names.is_none() {
            continue;
        }
        for call_profile in &profiles {
            if let Some(pair) = score_pair(def_profile, call_profile) {
                pairs.push(pair);
            }
        }
    }

    rank_pairs(&mut pairs);
    let outcome = select_outcome(&pairs);
    (outcome, pairs)
}

/// Scores one (def_profile, call_profile) pair, or `None` when either
/// exclusion (same-kind self-pairing, non-call-shaped callee, or
/// field-aliasing — see `extracted_target_kinds`'s and
/// `is_call_shaped_kind`'s docs) rules the pair out entirely.
fn score_pair(
    def_profile: &CandidateProfile,
    call_profile: &CandidateProfile,
) -> Option<PairCandidate> {
    let def_names = def_profile.def_names.as_ref()?;
    if call_profile.candidate.node_kind == def_profile.candidate.node_kind {
        return None;
    }
    if !call_profile.call_shaped {
        return None;
    }
    if def_profile
        .target_kinds
        .contains(&call_profile.candidate.node_kind)
        || call_profile
            .target_kinds
            .contains(&def_profile.candidate.node_kind)
    {
        return None;
    }

    let def_name_set: HashSet<&str> = def_names.iter().map(String::as_str).collect();
    let call_referenced: HashSet<&str> =
        call_profile.texts.iter().map(|t| last_segment(t)).collect();

    let call_total = call_profile.texts.len();
    let call_matched = call_profile
        .texts
        .iter()
        .filter(|t| def_name_set.contains(last_segment(t)))
        .count();
    let call_rate = call_matched as f64 / call_total as f64;

    let def_total = def_names.len();
    let def_matched = def_names
        .iter()
        .filter(|n| call_referenced.contains(n.as_str()))
        .count();
    let def_rate = def_matched as f64 / def_total as f64;

    Some(PairCandidate {
        def: def_profile.candidate.clone(),
        call: call_profile.candidate.clone(),
        def_matched,
        def_total,
        def_rate,
        call_matched,
        call_total,
        call_rate,
    })
}

/// Tie-break, after rate and total support: prefer a NAMED FIELD source
/// over the `FirstNamedChild` positional fallback on each side. A tie at
/// equal rate/support (e.g. Go's `call_expression.function` and
/// `call_expression`'s own first named child are the identical node) is
/// resolved toward the more specific, grammar-stable source.
fn rank_pairs(pairs: &mut [PairCandidate]) {
    let is_field = |c: &Candidate| matches!(c.source, Source::Field(_));
    pairs.sort_by(|a, b| {
        b.min_rate()
            .partial_cmp(&a.min_rate())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then((b.def_total + b.call_total).cmp(&(a.def_total + a.call_total)))
            .then((is_field(&b.def), is_field(&b.call)).cmp(&(is_field(&a.def), is_field(&a.call))))
    });
}

/// `MIN_PAIR_SUPPORT` is applied HERE, at selection time, not as an earlier
/// filter — every pair (including the low-support ones) stays in the
/// returned `pairs` list so a caller's diagnostic report can still see
/// them; only the WINNER is required to clear it.
fn select_outcome(pairs: &[PairCandidate]) -> PairOutcome {
    let eligible = pairs
        .iter()
        .find(|p| p.def_total >= MIN_PAIR_SUPPORT && p.call_total >= MIN_PAIR_SUPPORT);
    match eligible {
        Some(best)
            if best.def_rate >= RESOLUTION_THRESHOLD && best.call_rate >= RESOLUTION_THRESHOLD =>
        {
            PairOutcome::Resolved(best.clone())
        }
        Some(best) => PairOutcome::Unresolved {
            pairs_tried: pairs.len(),
            best_min_rate: best.min_rate(),
        },
        None => PairOutcome::Unresolved {
            pairs_tried: pairs.len(),
            best_min_rate: 0.0,
        },
    }
}
