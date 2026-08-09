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
use super::objc::OBJC_SPEC;
use super::python::PYTHON_SPEC;
use super::ruby::RUBY_SPEC;
use super::typescript::TS_SPEC;
// Only the (test-only) SHALLOW_SPECS table reads this, so it stays gated with
// it — an ungated import would be an unused-import warning in release builds.
use super::rust::RUST_SPEC;
#[cfg(test)]
use super::shallow::ShallowSpec;
use super::swift::SWIFT_SPEC;
use crate::parser::Language;
use tree_sitter::Language as TsLanguage;

/// Returns the spec row for a migrated language, or `None` if the language is
/// still served by its hand-written walker. Migrated so far (ADR-0055):
/// Go (phase 1), Python (phase 2), Java (phase 3), Kotlin (phase 4),
/// Swift (phase 5), C (phase 6), C++ (phase 7), ObjC, TypeScript, and Rust —
/// all ten core languages are now table-driven.
pub(crate) fn lang_spec(language: Language) -> Option<&'static LangSpec> {
    match language {
        Language::Go => Some(&GO_SPEC),
        Language::Python => Some(&PYTHON_SPEC),
        Language::Java => Some(&JAVA_SPEC),
        Language::Kotlin => Some(&KOTLIN_SPEC),
        Language::Swift => Some(&SWIFT_SPEC),
        Language::C => Some(&C_SPEC),
        Language::Cpp => Some(&CPP_SPEC),
        Language::ObjC => Some(&OBJC_SPEC),
        Language::TypeScript => Some(&TS_SPEC),
        Language::Rust => Some(&RUST_SPEC),
        // Every core language is migrated; any remaining `Language` variant
        // (e.g. the shallow-only Ruby row) has no deep spec.
        _ => None,
    }
}

/// Resolves the tree-sitter grammar for `language`/`file_path` — exactly the
/// grammar `parse_with_spec`/`parse_shallow` would select for the same pair.
/// The single grammar-selection table in the crate: full-AST capture
/// (`parser::ast_capture`) reuses this instead of adding a second per-language
/// match, so a language's grammar is named in exactly one place.
///
/// Every language resolves: the ten deep-spec rows via `lang_spec` (honoring
/// `ts_language_by_ext` for TypeScript's typescript/tsx split), and Ruby —
/// the one language with no deep spec — via its shallow row directly. This is
/// the one unavoidable branch: Ruby's grammar factory lives on a different
/// struct type (`ShallowSpec`, not `LangSpec`) because it is genuinely a
/// different extraction depth (ADR-0056), not new per-language logic.
pub(crate) fn grammar_for(language: Language, file_path: &str) -> Option<TsLanguage> {
    if let Some(spec) = lang_spec(language) {
        return Some(match spec.ts_language_by_ext {
            Some(select) => select(file_path),
            None => (spec.ts_language)(),
        });
    }
    if language == Language::Ruby {
        return Some((RUBY_SPEC.ts_language)());
    }
    None
}

#[cfg(test)]
mod grammar_for_tests {
    use super::grammar_for;
    use crate::parser::Language;

    /// Every `Language` variant must resolve a grammar — full-AST capture
    /// has nothing to fall back to if one is silently missing.
    #[test]
    fn every_language_resolves_a_grammar() {
        for lang in [
            Language::Rust,
            Language::Python,
            Language::TypeScript,
            Language::Java,
            Language::Kotlin,
            Language::Swift,
            Language::ObjC,
            Language::C,
            Language::Cpp,
            Language::Go,
            Language::Ruby,
        ] {
            assert!(
                grammar_for(lang, "f.txt").is_some(),
                "{lang:?} must resolve a tree-sitter grammar"
            );
        }
    }

    /// TypeScript's `.tsx`/`.ts` split must route through `ts_language_by_ext`
    /// here exactly as it does in `parse_with_spec` — a JSX construct only
    /// parses under the tsx grammar, so picking the wrong one would make the
    /// completeness test see spurious ERROR nodes.
    #[test]
    fn typescript_grammar_selection_honors_file_extension() {
        let ts = grammar_for(Language::TypeScript, "a.ts").unwrap();
        let tsx = grammar_for(Language::TypeScript, "a.tsx").unwrap();
        // `tree_sitter::Language` has no public identity/Debug API to compare
        // directly; its `node_kind_count()` is a real structural signal —
        // the tsx grammar adds JSX node kinds the plain ts grammar lacks, so
        // an equal count here would mean the extension routing collapsed to
        // one grammar (exactly the bug `ts_language_by_ext` exists to avoid).
        assert_ne!(
            ts.node_kind_count(),
            tsx.node_kind_count(),
            ".ts and .tsx must select different grammars (JSX kinds only in tsx)"
        );
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
    &OBJC_SPEC,
    &TS_SPEC,
    &RUST_SPEC,
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

    /// Pins the strangler-fig migration state: every core language now resolves
    /// to its spec (so the embedded-reparse path and any registry consumer sees
    /// it) — with Rust's migration the core set is complete. Kills the "delete
    /// the `Language::Python` arm" mutant, which `parse_file`'s direct
    /// `PYTHON_SPEC` dispatch would otherwise hide.
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
        assert!(lang_spec(Language::ObjC).is_some(), "ObjC is migrated");
        assert!(
            lang_spec(Language::TypeScript).is_some(),
            "TypeScript is migrated"
        );
        assert!(
            lang_spec(Language::Rust).is_some(),
            "Rust is migrated (phase 8)"
        );
    }
}
