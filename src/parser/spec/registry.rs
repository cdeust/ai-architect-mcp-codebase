// parser::spec::registry — the composition root for the table-driven parser
// (ADR-0055 §1, mirroring the existing `LanguageProvider` registry). Maps a
// `Language` to its `&'static LangSpec`, or `None` for a language not yet
// migrated off its hand-written walker (strangler-fig: one language at a time).

use super::go::GO_SPEC;
use super::java::JAVA_SPEC;
use super::kotlin::KOTLIN_SPEC;
use super::lang_spec::LangSpec;
use super::python::PYTHON_SPEC;
use super::swift::SWIFT_SPEC;
use crate::parser::Language;

/// Returns the spec row for a migrated language, or `None` if the language is
/// still served by its hand-written walker. Migrated so far (ADR-0055):
/// Go (phase 1), Python (phase 2), Java (phase 3), Kotlin (phase 4),
/// Swift (phase 5).
pub(crate) fn lang_spec(language: Language) -> Option<&'static LangSpec> {
    match language {
        Language::Go => Some(&GO_SPEC),
        Language::Python => Some(&PYTHON_SPEC),
        Language::Java => Some(&JAVA_SPEC),
        Language::Kotlin => Some(&KOTLIN_SPEC),
        Language::Swift => Some(&SWIFT_SPEC),
        // The remaining five stay on their hand-written walkers until each is
        // migrated at parity behind the accuracy gate (ADR-0055 §5).
        _ => None,
    }
}

/// All migrated spec rows, for the spec-validation guard to iterate. Only the
/// guard (a `#[cfg(test)]` consumer) reads this, so it is test-only — gating it
/// keeps production builds free of an otherwise-unused const (§9).
#[cfg(test)]
pub(crate) const MIGRATED_SPECS: &[&LangSpec] = &[
    &GO_SPEC,
    &PYTHON_SPEC,
    &JAVA_SPEC,
    &KOTLIN_SPEC,
    &SWIFT_SPEC,
];

#[cfg(test)]
mod tests {
    use super::lang_spec;
    use crate::parser::Language;

    /// Pins the strangler-fig migration state: every migrated language resolves
    /// to its spec (so the embedded-reparse path and any registry consumer sees
    /// it), and an un-migrated language resolves to `None` (still on its hand-
    /// written walker). Kills the "delete the `Language::Python` arm" mutant,
    /// which `parse_file`'s direct `PYTHON_SPEC` dispatch would otherwise hide.
    #[test]
    fn migrated_languages_resolve_and_unmigrated_do_not() {
        assert!(
            lang_spec(Language::Go).is_some(),
            "Go is migrated (phase 1)"
        );
        assert!(
            lang_spec(Language::Python).is_some(),
            "Python is migrated (phase 2)"
        );
        assert!(
            lang_spec(Language::Java).is_some(),
            "Java is migrated (phase 3)"
        );
        assert!(
            lang_spec(Language::Kotlin).is_some(),
            "Kotlin is migrated (phase 4)"
        );
        assert!(
            lang_spec(Language::Swift).is_some(),
            "Swift is migrated (phase 5)"
        );
        // Rust is not migrated yet — must stay on its hand-written walker.
        assert!(
            lang_spec(Language::Rust).is_none(),
            "Rust is not migrated; registry must return None"
        );
    }
}
