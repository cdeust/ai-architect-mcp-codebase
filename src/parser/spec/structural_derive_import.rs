// parser::spec::structural_derive_import — the IMPORT half of issue #232's
// rule auto-derivation, split out of `structural_derive.rs` purely for the
// coding-standards.md §4.1 500-line file cap (that module was pushing past
// it once the joint definition/call pairing landed). No new mechanism: this
// answers "does a candidate's extracted path resolve to a real file in the
// corpus?", independently of the definition/call pairing — an import path
// is validated against the corpus's own file system, not against another
// candidate, so it carries none of the circularity risk that motivated
// deriving definitions and calls jointly (see that module's doc).

use std::collections::HashSet;
use std::path::Path;

use super::structural_derive::{
    extracted_texts, observed_candidates, CorpusFile, DerivationOutcome, ScoredCandidate,
    RESOLUTION_THRESHOLD,
};

fn strip_quotes(text: &str) -> String {
    text.trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_string()
}

/// Whether `raw` (after quote-stripping) resolves to a file that actually
/// exists in the corpus — a bare relative/suffix match against every
/// corpus-file path, or the same with one of the corpus's OWN observed file
/// extensions appended (never a per-language extension list: the
/// extensions come from `corpus_extensions`, itself read off the corpus's
/// own file names).
fn resolves_to_corpus_file(
    raw: &str,
    corpus_paths: &HashSet<String>,
    extensions: &HashSet<String>,
) -> bool {
    let candidate = strip_quotes(raw);
    if candidate.is_empty() {
        return false;
    }
    let matches = |p: &str| p == candidate || p.ends_with(&format!("/{candidate}"));
    if corpus_paths.iter().any(|p| matches(p)) {
        return true;
    }
    extensions.iter().any(|ext| {
        let with_ext = format!("{candidate}.{ext}");
        corpus_paths
            .iter()
            .any(|p| p == &with_ext || p.ends_with(&format!("/{with_ext}")))
    })
}

fn corpus_extensions(files: &[CorpusFile]) -> HashSet<String> {
    files
        .iter()
        .filter_map(|f| {
            Path::new(&f.rel_path)
                .extension()
                .map(|e| e.to_string_lossy().to_string())
        })
        .collect()
}

/// Derives the IMPORT rule: for every (node kind, source) candidate, extract
/// a path text and score by whether it resolves to a real file in the
/// corpus. A language with no import-equivalent construct (SQL, verified in
/// `structural_held_out_sample_tests`) is expected to return `Unresolved`
/// here — that is the correct, honest answer, not a bug in this function.
pub(crate) fn derive_import_rule(
    files: &[CorpusFile],
) -> (DerivationOutcome, Vec<ScoredCandidate>) {
    let corpus_paths: HashSet<String> = files.iter().map(|f| f.rel_path.clone()).collect();
    let extensions = corpus_extensions(files);
    let mut scored = Vec::new();
    for candidate in observed_candidates(files) {
        let texts = extracted_texts(files, &candidate);
        if texts.is_empty() {
            continue;
        }
        let total = texts.len();
        let matched = texts
            .iter()
            .filter(|t| resolves_to_corpus_file(t, &corpus_paths, &extensions))
            .count();
        scored.push(ScoredCandidate {
            candidate,
            matched,
            total,
            rate: matched as f64 / total as f64,
        });
    }
    scored.sort_by(|a, b| {
        b.rate
            .partial_cmp(&a.rate)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.total.cmp(&a.total))
    });
    let outcome = match scored.first() {
        Some(best) if best.rate >= RESOLUTION_THRESHOLD => {
            DerivationOutcome::Resolved(best.clone())
        }
        Some(best) => DerivationOutcome::Unresolved {
            candidates_tried: scored.len(),
            best_rate: best.rate,
        },
        None => DerivationOutcome::Unresolved {
            candidates_tried: 0,
            best_rate: 0.0,
        },
    };
    (outcome, scored)
}
