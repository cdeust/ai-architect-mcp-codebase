// language_provider — per-language resolution conventions for Stage 3b.
//
// The resolver's PHASE logic (imports/calls/implements/extends/uses) is
// language-agnostic: it operates on graph node LABELS (Import, CallSite,
// Method, Struct, …) that every parser emits uniformly, and on qualified
// names that every parser normalizes to `::` (verified: parser/{rust,go,
// java,kotlin,swift,objc,c,cpp,typescript,python}.rs all build QNs with
// `::`). What DIVERGES per language is a small set of surface conventions:
//
//   1. file extensions          (file-id extraction from node ids/QNs)
//   2. import-path separator     — as STORED after parsing, not as written in
//                                 source. The live parsers (Rust/Python/TS)
//                                 normalize to `::`; the dormant parsers keep
//                                 raw separators (`.` Java/Kotlin; `/` Go/
//                                 ObjC/C/C++; bare module name for Swift).
//   3. import-path prefix to strip (Rust `crate::`)
//   4. external stdlib/framework roots (skip-resolution set)
//   5. primitive/builtin type names    (skip in type-usage resolution)
//   6. derive/macro expansion table key (Rust derives only)
//
// This module isolates those six divergences behind a trait + registry so
// the seven parse-wired-but-resolution-dormant grammars (Java, Kotlin,
// Swift, ObjC, C, C++, Go) resolve cross-file edges with no change to the
// phase logic. Mirrors the existing macro_expansion::get_macro_table
// trait+registry pattern.
//
// source: stages/stage-3b.md §5 (phase contract); per-constant sources cited
// at each provider. Registered in BOTH lib.rs and main.rs (lib+bin crate).

mod file_prefix;
mod kotlin_prefixes;
mod providers;

// Re-exported so every existing `language_provider::extract_file_prefix*`
// path stays valid after the split.
pub use file_prefix::*;
pub use providers::*;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Per-language resolution conventions. Concrete impls are zero-sized unit
/// structs holding only `&'static` data; the registry returns shared refs.
pub trait LanguageProvider: Send + Sync {
    /// Canonical language tag (matches parser::Language::as_str). Used by the
    /// registry round-trip test and as the provider identity accessor.
    #[allow(dead_code)] // exercised by tests; identity contract for the registry.
    fn language(&self) -> &'static str;

    /// Separator used in this language's IMPORT PATHS — NOT in qualified
    /// names (QNs are uniformly `::` across every parser). Used to take the
    /// last path segment when matching an import/callee against the symbol
    /// index. Default `::`.
    fn import_separator(&self) -> &'static str {
        "::"
    }

    /// Strip a leading language-specific path prefix that is not part of any
    /// symbol's qualified name (e.g. Rust `crate::`). Borrow-only; default is
    /// identity. source: resolver.rs::normalize_import_path (Rust behavior).
    fn normalize_import_path<'a>(&self, path: &'a str) -> &'a str {
        path
    }

    /// Well-known standard-library / framework path roots that are NOT part
    /// of the indexed corpus. A reference whose first path segment matches is
    /// skipped (recorded as an "external" UnresolvedRef) instead of being
    /// matched against in-corpus symbols. Mirrors the existing Rust/Python/
    /// Node "common subset" lists — intentionally not exhaustive.
    fn external_prefixes(&self) -> &'static [&'static str];

    /// Primitive / builtin type names to skip during type-usage resolution
    /// (they are not corpus symbols). Only names that survive the uppercase-
    /// identifier convention in resolver::extract_type_identifiers need be
    /// listed; lowercase builtins (e.g. Go `int`, C `char`) are already
    /// skipped by that convention.
    fn primitives(&self) -> &'static [&'static str];

    /// macro_expansion table key for derive/decorator → trait expansion, if
    /// the language has one. `None` is the honest answer for languages with
    /// no such mechanism — we do not fabricate stdlib edges for them.
    fn derive_macro_key(&self) -> Option<&'static str> {
        None
    }

    /// Package/module grouping for a file id (PackageProximity evidence +
    /// package-keyed ImportMatch, issue #29). `None` is the honest default —
    /// returning `None` for a language is what preserves its pre-issue-#29
    /// resolution behavior unchanged (see call_evidence.rs). Override only
    /// when the directory is a reliable package/module proxy.
    fn package_of(&self, _file_id: &str) -> Option<String> {
        None
    }

    /// Last segment of an import path or callee, after normalization and
    /// separator split. Provided; do not override.
    fn import_last_segment<'a>(&self, path: &'a str) -> &'a str {
        let p = self.normalize_import_path(path);
        p.rsplit(self.import_separator()).next().unwrap_or(p)
    }

    /// pre: `path` uses this provider's `import_separator`. post: true iff
    /// `path` starts, segment-by-segment, with an `external_prefixes()`
    /// entry. A `.`-containing entry (e.g. `"com.google"`) matches as a
    /// compound root only when the separator is also `.` (Java/Kotlin);
    /// otherwise it matches atomically (preserves C's `"stdio.h"`).
    fn is_external_import(&self, path: &str) -> bool {
        let sep = self.import_separator();
        // crate/self/super and leading-dot relatives are always internal.
        let first = path.split(sep).next().unwrap_or(path);
        if first == "crate" || first == "self" || first == "super" {
            return false;
        }
        if first.starts_with('.') {
            return false;
        }
        let path_segments: Vec<&str> = path.split(sep).collect();
        self.external_prefixes().iter().any(|prefix| {
            if sep == "." && prefix.contains('.') {
                let prefix_segments: Vec<&str> = prefix.split('.').collect();
                path_segments.len() >= prefix_segments.len()
                    && path_segments[..prefix_segments.len()] == prefix_segments[..]
            } else {
                first == *prefix
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

static DEFAULT: DefaultProvider = DefaultProvider;
static RUST: RustProvider = RustProvider;
static PYTHON: PythonProvider = PythonProvider;
static TYPESCRIPT: TypeScriptProvider = TypeScriptProvider;
static JAVA: JavaProvider = JavaProvider;
static KOTLIN: KotlinProvider = KotlinProvider;
static SWIFT: SwiftProvider = SwiftProvider;
static OBJC: ObjCProvider = ObjCProvider;
static C: CProvider = CProvider;
static CPP: CppProvider = CppProvider;
static GO: GoProvider = GoProvider;

/// Resolve a provider for a `language` column value. Unknown / empty / the
/// kuzu `Null(String)` sentinel fall back to the conservative DefaultProvider
/// so resolution never aborts on a missing language tag.
/// source: parser::Language::as_str for the canonical tags.
pub fn provider_for(language: &str) -> &'static dyn LanguageProvider {
    match language {
        "rust" => &RUST,
        "python" => &PYTHON,
        "typescript" => &TYPESCRIPT,
        "java" => &JAVA,
        "kotlin" => &KOTLIN,
        "swift" => &SWIFT,
        "objc" => &OBJC,
        "c" => &C,
        "cpp" => &CPP,
        "go" => &GO,
        _ => &DEFAULT,
    }
}
