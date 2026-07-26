// parser::spec::registry — the composition root for the table-driven parser
// (ADR-0055 §1, mirroring the existing `LanguageProvider` registry). Maps a
// `Language` to its `&'static LangSpec`, or `None` for a language not yet
// migrated off its hand-written walker (strangler-fig: one language at a time).

use super::go::GO_SPEC;
use super::lang_spec::LangSpec;
use crate::parser::Language;

/// Returns the spec row for a migrated language, or `None` if the language is
/// still served by its hand-written walker. Phase 1 (ADR-0055): Go only.
pub(crate) fn lang_spec(language: Language) -> Option<&'static LangSpec> {
    match language {
        Language::Go => Some(&GO_SPEC),
        // The remaining nine stay on their hand-written walkers until each is
        // migrated at parity behind the accuracy gate (ADR-0055 §5).
        _ => None,
    }
}

/// All migrated spec rows, for the spec-validation guard to iterate. Only the
/// guard (a `#[cfg(test)]` consumer) reads this, so it is test-only — gating it
/// keeps production builds free of an otherwise-unused const (§9).
#[cfg(test)]
pub(crate) const MIGRATED_SPECS: &[&LangSpec] = &[&GO_SPEC];
