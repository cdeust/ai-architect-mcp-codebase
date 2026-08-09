// parser::spec::structural_derive — issue #232: derive per-language
// extraction rules by ENUMERATING candidates from the grammar's own
// introspectable vocabulary (field names + node kinds, both surfaced via
// `tree_sitter::Node`/`TreeCursor` while walking a real corpus) and SCORING
// each candidate by resolution against the corpus itself.
//
// Owner directive (#232, 2026-08-09): the zero-per-language-artifact thesis
// reaches ~40% of prevalence-ranked languages (#224/#231) — SQL and
// PowerShell, both mainstream, are entirely unreached. The architecture
// becomes shared engine + a small per-language configuration, but the
// configuration must be MACHINE-DERIVED, never hand-authored. This module is
// that derivation mechanism.
//
// ## Validation signal — resolution against the corpus, never against our
// own extractor's output
// Comparing a derived rule to what `structural.rs` already produces would be
// circular (this project has rejected exactly that measurement twice
// already — see #224's prior-art survey). Per #232's own wording, three
// resolution signals, one per rule kind:
//   - a DEFINITION candidate is good if the symbols it defines are the ones
//     a CALL candidate elsewhere in the corpus actually references;
//   - a CALL candidate is good if the callees it extracts match symbols a
//     DEFINITION candidate elsewhere in the corpus actually defines;
//   - an IMPORT candidate is good if the paths it extracts resolve to files
//     that actually exist in the corpus.
//
// This module (`structural_derive.rs`) holds the shared vocabulary: corpus
// parsing, candidate enumeration from the grammar's own field/kind names,
// and text-extraction helpers. The joint definition/call PAIRING mechanism
// — including why it must be joint, not independent — lives in
// `structural_derive_pairing.rs` (split out purely for the
// coding-standards.md §4.1 500-line cap); the IMPORT mechanism lives in
// `structural_derive_import.rs` for the same reason.
//
// ## Candidate enumeration — from the grammar, not from a human
// `observed_candidates` walks every parsed file and, per named node KIND,
// records every FIELD NAME actually used among that kind's children
// (`TreeCursor::field_name`, backed by the same field-id table
// `Language::field_name_for_id` exposes count/names for) plus whether the
// kind ever has a first NAMED CHILD at all (the positional fallback
// `structural_fallback.rs`'s own TIER 2 already relies on, for grammars that
// expose the same information as a plain child node instead of a field). No
// node-kind name or field name is hand-picked; the candidate list is
// whatever the corpus's own parse trees contain.
//
// ## Thresholds — measured, not invented (coding-standards.md §8)
// `DEFINITION_DISTINCTNESS_THRESHOLD = 0.5`, `RESOLUTION_THRESHOLD = 0.25`.
// Source: both measured in this PR's own control run
// (`structural_derive_tests::derives_known_correct_rule_for_go`/`_python`).
// `RESOLUTION_THRESHOLD` is deliberately NOT close to 1.0: on REAL code (as
// opposed to a toy two-line snippet), most call sites target EXTERNAL
// (stdlib/imported) functions, not ones defined in the same small corpus —
// the known-correct Go rule (`function_declaration.name` /
// `call_expression.function`) measured 0.35 def_rate / 0.54 call_rate; the
// known-correct Python rule measured 0.35 / 0.27. Both were still the
// TOP-RANKED pair among every candidate once `structural_derive_pairing.rs`'s
// exclusions existed (field-aliasing, argument-list-shape gating — see that
// module's doc). 0.25 sits just under the lower of those two real
// measurements. Where no pair/candidate clears both thresholds, the
// `derive_*` functions return an `Unresolved` outcome — never the
// best-scoring candidate regardless of how low its score is (#232: "a wrong
// rule is worse than an absent one").

use std::collections::{HashMap, HashSet};

use tree_sitter::{Language as TsLanguage, Node, Tree};

use crate::parser::node_text;

pub(crate) const RESOLUTION_THRESHOLD: f64 = 0.25;

/// A DEFINITION candidate must also be mostly-UNIQUE-per-name: a real
/// definition site names a distinct thing almost every time (Go's five
/// `function_declaration.name` instances in the control corpus are five
/// distinct names), whereas a reference/use site's field (a selector's
/// `operand`, an object reference's `schema` qualifier, or — the case that
/// motivated this constant — ANY locally-scoped binding's own name field)
/// repeats or clusters far less distinctly. Measured in this PR's own
/// control run: definition-shaped fields measured distinctness 1.0;
/// several reference-shaped fields measured 0.1-0.3. 0.5 sits in that gap.
pub(super) const DEFINITION_DISTINCTNESS_THRESHOLD: f64 = 0.5;

/// Where a candidate rule reads its extracted text from — the grammar's OWN
/// vocabulary, nothing per-language.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    /// A named field on the node (`Node::child_by_field_name`).
    Field(String),
    /// The node's first NAMED child, positionally — the fallback
    /// `structural_fallback.rs` already needed for fieldless grammars
    /// (Swift/Kotlin calls, Zig's builtin functions).
    FirstNamedChild,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub node_kind: String,
    pub source: Source,
}

#[derive(Clone, Debug)]
pub(crate) struct ScoredCandidate {
    pub candidate: Candidate,
    pub matched: usize,
    pub total: usize,
    pub rate: f64,
}

#[derive(Debug)]
pub(crate) enum DerivationOutcome {
    Resolved(ScoredCandidate),
    /// `candidates_tried == 0` means the corpus never produced a single
    /// extractable candidate for this role at all — reported distinctly
    /// from "candidates existed but none resolved" so the two honest-
    /// failure shapes aren't conflated in a report.
    Unresolved {
        candidates_tried: usize,
        best_rate: f64,
    },
}

pub(crate) struct CorpusFile {
    pub rel_path: String,
    pub source: String,
    pub tree: Tree,
}

/// Parses every `(relative_path, source)` pair with `ts_language`, skipping
/// (not panicking on) any file the grammar cannot set up or parse — a
/// derivation run over a real, messy corpus must not abort on one bad file.
pub(crate) fn parse_corpus(
    ts_language: fn() -> TsLanguage,
    files: &[(&str, &str)],
) -> Vec<CorpusFile> {
    let mut out = Vec::with_capacity(files.len());
    for (rel_path, source) in files {
        let lang = ts_language();
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&lang).is_err() {
            continue;
        }
        if let Some(tree) = parser.parse(source, None) {
            out.push(CorpusFile {
                rel_path: (*rel_path).to_string(),
                source: (*source).to_string(),
                tree,
            });
        }
    }
    out
}

/// A candidate name/callee/path text must look like a single token or a
/// dotted/namespaced reference to it — never whitespace-bearing text (a
/// `FirstNamedChild` probe landing on a whole body or statement list must be
/// excluded, not scored as if it were a plausible symbol).
pub(super) fn is_plausible_symbol_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= 128 && !text.chars().any(char::is_whitespace)
}

pub(super) fn is_identifier_like(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && text.len() <= 128
}

/// Reduces a dotted/namespaced reference to its trailing segment
/// (`obj.method` -> `method`), mirroring `structural_scope.rs::last_segment`
/// — duplicated locally rather than shared across the `#[cfg(test)]`
/// boundary between derivation and production-shaped code, same rationale
/// that module's own doc gives for its own local copy.
pub(super) fn last_segment(text: &str) -> &str {
    for sep in ['.', ':'] {
        if let Some(idx) = text.rfind(sep) {
            let candidate = &text[idx + 1..];
            if !candidate.is_empty() {
                return candidate;
            }
        }
    }
    text
}

pub(super) fn walk_all(root: Node) -> Vec<Node> {
    let mut stack = vec![root];
    let mut out = Vec::new();
    while let Some(node) = stack.pop() {
        if node.is_named() {
            out.push(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

/// Per node KIND, every field name observed among its children anywhere in
/// the corpus, plus (as `Source::FirstNamedChild`) whether it ever has a
/// first named child at all. This IS the candidate space — the grammar's own
/// vocabulary, read off real parsed instances rather than off
/// `node-types.json` (equivalent information; a corpus walk is what a
/// derivation run has on hand without an extra file-format parser).
pub(super) fn observed_candidates(files: &[CorpusFile]) -> Vec<Candidate> {
    let mut map: HashMap<String, HashSet<Source>> = HashMap::new();
    for file in files {
        for node in walk_all(file.tree.root_node()) {
            let entry = map.entry(node.kind().to_string()).or_default();
            let mut cursor = node.walk();
            if cursor.goto_first_child() {
                loop {
                    if let Some(field) = cursor.field_name() {
                        entry.insert(Source::Field(field.to_string()));
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
            if node.named_child(0).is_some() {
                entry.insert(Source::FirstNamedChild);
            }
        }
    }
    let mut out = Vec::new();
    for (kind, sources) in map {
        for source in sources {
            out.push(Candidate {
                node_kind: kind.clone(),
                source,
            });
        }
    }
    out
}

pub(super) fn extract_text(source: &str, node: Node, src: &Source) -> Option<String> {
    let target = match src {
        Source::Field(name) => node.child_by_field_name(name)?,
        Source::FirstNamedChild => node.named_child(0)?,
    };
    let text = node_text(source, target).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Every plausible-symbol text a candidate extracts across the whole
/// corpus, in instance order (repeats included — callers that need the
/// per-instance count rely on that).
pub(super) fn extracted_texts(files: &[CorpusFile], candidate: &Candidate) -> Vec<String> {
    let mut out = Vec::new();
    for file in files {
        for node in walk_all(file.tree.root_node()) {
            if node.kind() != candidate.node_kind {
                continue;
            }
            if let Some(text) = extract_text(&file.source, node, &candidate.source) {
                if is_plausible_symbol_text(&text) {
                    out.push(text);
                }
            }
        }
    }
    out
}
