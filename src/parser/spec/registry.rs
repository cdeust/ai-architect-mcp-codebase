// parser::spec::registry — the composition root for the table-driven parser
// (ADR-0055 §1, mirroring the existing `LanguageProvider` registry). Maps a
// `Language` to its `&'static LangSpec`, or `None` for a language not yet
// migrated off its hand-written walker (strangler-fig: one language at a time).

use super::c::C_SPEC;
use super::cpp::CPP_SPEC;
use super::go::GO_SPEC;
use super::java::JAVA_SPEC;
use super::kotlin::KOTLIN_SPEC;
use super::lang_spec::LangSpec;
use super::python::PYTHON_SPEC;
// Only the (test-only) SHALLOW_SPECS table reads these, so they are gated with
// it — an ungated import would be an unused-import warning in release builds.
#[cfg(test)]
use super::ruby::RUBY_SPEC;
#[cfg(test)]
use super::shallow::ShallowSpec;
use super::swift::SWIFT_SPEC;
use crate::parser::Language;

/// Returns the spec row for a migrated language, or `None` if the language is
/// still served by its hand-written walker. Migrated so far (ADR-0055):
/// Go (phase 1), Python (phase 2), Java (phase 3), Kotlin (phase 4),
/// Swift (phase 5), C (phase 6), C++ (phase 7).
pub(crate) fn lang_spec(language: Language) -> Option<&'static LangSpec> {
    match language {
        Language::Go => Some(&GO_SPEC),
        Language::Python => Some(&PYTHON_SPEC),
        Language::Java => Some(&JAVA_SPEC),
        Language::Kotlin => Some(&KOTLIN_SPEC),
        Language::Swift => Some(&SWIFT_SPEC),
        Language::C => Some(&C_SPEC),
        Language::Cpp => Some(&CPP_SPEC),
        // The remaining three stay on their hand-written walkers until each is
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
    &C_SPEC,
    &CPP_SPEC,
];

/// All shallow spec rows (ADR-0056), for the guard to iterate. A shallow row
/// gets the same node-kind validation as a deep one: breadth must not come
/// with weaker §8 enforcement.
#[cfg(test)]
pub(crate) const SHALLOW_SPECS: &[&ShallowSpec] = &[&RUBY_SPEC];

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
        assert!(lang_spec(Language::C).is_some(), "C is migrated (phase 6)");
        assert!(
            lang_spec(Language::Cpp).is_some(),
            "C++ is migrated (phase 7)"
        );
        // Rust is not migrated yet — must stay on its hand-written walker.
        assert!(
            lang_spec(Language::Rust).is_none(),
            "Rust is not migrated; registry must return None"
        );
    }
}
