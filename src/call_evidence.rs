// call_evidence — Kotlin (and future #seq-using languages') evidence
// gathering for the shared ambiguity policy (ambiguity_policy.rs, issue
// #30). Pure orchestration: builds Candidate representations and Context
// values, then delegates the actual ambiguity/confidence decision to
// ambiguity_policy::resolve. Does not change that policy's logic.
//
// source: issue #29 — two independent defects blocked Kotlin call
// resolution even after the parser stopped stripping receivers:
//   1. Several dormant-language parsers (kotlin.rs, java.rs, go.rs, ...)
//      append a `#<seq>` disambiguator to a symbol's qualified_name to keep
//      overloaded same-named functions unique for the graph's primary key.
//      ambiguity_policy::import_matches does an exact-suffix comparison
//      against import/callee text, which never contains a numeric seq —
//      so ImportMatch could never fire for any of those languages.
//   2. A candidate's qualified_name is `file_path::name#seq`: a real,
//      package-qualified import/callee (`pkg.symbol`) can never suffix-
//      match it, because the file's own name sits between the package-like
//      directory prefix and the symbol name. Embedding the real `package`
//      declaration into the qualified_name to fix this would break
//      indexer/persist.rs's File-node linkage, which keys top-level
//      Defines/Imports edges on the *exact* file path (see
//      language_provider::KotlinProvider::package_of's doc comment) — out
//      of scope for a call-resolution fix.
//
// Both are fixed here, entirely on the resolver side: `strip_seq_suffix`
// undoes (1); the two-pass evidence gathering in `resolve_two_pass` works
// around (2) by trying a package-keyed representation (directory-of-file
// substituted for the file path) before falling back to the real,
// file-based representation used pre-issue-#29. Languages whose provider
// does not implement `package_of` (i.e. returns `None`) get an inert first
// pass (see `resolve_two_pass` doc comment) — zero behavior change for
// Rust/Python/TypeScript.

use crate::ambiguity_policy::{self, Candidate, Context as PolicyContext, Resolution};
use crate::language_provider::LanguageProvider;

/// Strips a trailing `#<digits>` disambiguator from a qualified name, for
/// MATCHING purposes only — never touches the real stored qualified_name/id,
/// so DB inserts keep the uniqueness suffix and never collide.
/// postcondition: returns `qn` unchanged unless it ends in `#` followed by
/// one or more ASCII digits, in which case returns the prefix before `#`.
pub(crate) fn strip_seq_suffix(qn: &str) -> &str {
    match qn.rsplit_once('#') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => qn,
    }
}

/// Evidence available at a call site, gathered by resolver.rs.
pub(crate) struct CallEvidence<'a> {
    /// Import paths in scope, or (for an already-qualified callee) the
    /// callee's own spelling — mirrors ambiguity_policy::Context.
    pub imports_hint: &'a [String],
    /// The caller's file id (as resolver.rs computes it today).
    pub caller_file: &'a str,
}

/// A candidate paired with an override match key, so a single generic
/// candidate type `T` (resolver.rs's SymbolEntry) can be matched under two
/// different string representations without ambiguity_policy.rs knowing
/// anything about `T`'s real shape.
#[derive(Clone)]
struct KeyedCandidate<T: Clone> {
    target: T,
    key: String,
}

impl<T: Clone> Candidate for KeyedCandidate<T> {
    fn qualified_name(&self) -> &str {
        &self.key
    }
}

/// precondition: `candidates` has 2+ entries (0/1 is already handled by the
/// caller's name-index lookup and ambiguity_policy::resolve's own len==1
/// fast path, which either pass reaches transparently).
/// postcondition: identical decision procedure to a single call of
/// ambiguity_policy::resolve, just tried against two representations in
/// preference order — never invents a resolution neither pass's evidence
/// tiers would have produced on their own.
///
/// Pass 1 (package-keyed): key = `package_of(candidate_file)::bare_name`,
/// evaluated with `imports_hint` as the only evidence (caller_file/-package
/// evidence suppressed by construction below) — this is the ONLY pass that
/// can match a real package-qualified callee/import against a candidate
/// declared in a *different* file of the same package. When `package_of`
/// returns `None` for every candidate (true for every language except
/// Kotlin today), every candidate's key degenerates to the identical bare
/// name string, so `unique_match` (ambiguity_policy.rs) can never call it
/// unique for 2+ candidates — this pass is then a guaranteed no-op and
/// falls straight through to Pass 2 (see call_evidence.rs module doc).
///
/// Pass 2 (file-keyed): key = the candidate's real qualified_name with only
/// the `#seq` disambiguator stripped — bit-for-bit what resolve_single_call
/// used pre-issue-#29, so SameFileUnique / directory-based PackageProximity
/// behave exactly as before for every language.
pub(crate) fn resolve_two_pass<T: Clone>(
    candidates: &[T],
    qn_of: impl Fn(&T) -> String,
    file_of: impl Fn(&T) -> String,
    provider: &dyn LanguageProvider,
    ev: &CallEvidence,
) -> Resolution<T> {
    if let Some(r) = try_package_pass(candidates, &qn_of, &file_of, provider, ev) {
        return r;
    }
    file_pass(candidates, &qn_of, provider, ev)
}

fn bare_name(qn: &str) -> String {
    strip_seq_suffix(qn.rsplit("::").next().unwrap_or(qn)).to_string()
}

fn try_package_pass<T: Clone>(
    candidates: &[T],
    qn_of: &impl Fn(&T) -> String,
    file_of: &impl Fn(&T) -> String,
    provider: &dyn LanguageProvider,
    ev: &CallEvidence,
) -> Option<Resolution<T>> {
    let keyed: Vec<KeyedCandidate<T>> = candidates
        .iter()
        .map(|c| {
            let name = bare_name(&qn_of(c));
            let key = match provider.package_of(&file_of(c)) {
                Some(pkg) => format!("{pkg}::{name}"),
                None => name,
            };
            KeyedCandidate {
                target: c.clone(),
                key,
            }
        })
        .collect();
    // caller_file/-package deliberately suppressed: this pass tests ONLY
    // import/callee-spelling evidence (ImportMatch / UniqueGlobal), never
    // SameFileUnique or PackageProximity — Pass 2 owns those, using the
    // real file-based representation.
    let ctx = PolicyContext {
        imports_in_scope: ev.imports_hint,
        caller_file: "",
        caller_package: None,
    };
    match ambiguity_policy::resolve(&keyed, &ctx) {
        Resolution::Resolved {
            target,
            evidence,
            confidence,
        } => Some(Resolution::Resolved {
            target: target.target,
            evidence,
            confidence,
        }),
        _ => None,
    }
}

fn file_pass<T: Clone>(
    candidates: &[T],
    qn_of: &impl Fn(&T) -> String,
    provider: &dyn LanguageProvider,
    ev: &CallEvidence,
) -> Resolution<T> {
    let keyed: Vec<KeyedCandidate<T>> = candidates
        .iter()
        .map(|c| KeyedCandidate {
            target: c.clone(),
            key: strip_seq_suffix(&qn_of(c)).to_string(),
        })
        .collect();
    let pkg = provider.package_of(ev.caller_file);
    let ctx = PolicyContext {
        imports_in_scope: ev.imports_hint,
        caller_file: ev.caller_file,
        caller_package: pkg.as_deref(),
    };
    match ambiguity_policy::resolve(&keyed, &ctx) {
        Resolution::Resolved {
            target,
            evidence,
            confidence,
        } => Resolution::Resolved {
            target: target.target,
            evidence,
            confidence,
        },
        Resolution::Ambiguous { candidates } => Resolution::Ambiguous {
            candidates: candidates.into_iter().map(|k| k.target).collect(),
        },
        Resolution::NotFound => Resolution::NotFound,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The segments of a `::`-qualified callee, turbofish and generic arguments
/// removed: `io::BufWriter::new` gives `["io", "BufWriter", "new"]`,
/// `Vec::<u8>::with_capacity` gives `["Vec", "with_capacity"]`. `None` for a
/// callee that is not a plain path (`x.len`, `(a + b).m`, `f`).
pub fn callee_path_segments(callee: &str) -> Option<Vec<&str>> {
    if !callee.contains("::") || callee.contains(['.', '(', '[']) {
        return None;
    }
    let segments: Vec<&str> = callee
        .split("::")
        .map(|s| s.split('<').next().unwrap_or(s))
        .filter(|s| !s.is_empty())
        .collect();
    (segments.len() >= 2).then_some(segments)
}

/// The qualifier a `::`-qualified callee names before its last segment, which
/// is the type of an associated call: `TaskSet::new` and `crate::a::TaskSet::new`
/// give `TaskSet`, `io::BufWriter::new` gives `BufWriter`, `Vec::<u8>::new`
/// gives `Vec`. For a free function reached through a module path the
/// qualifier is the module (`a::helper` gives `a`). `None` when the callee is
/// not a path. Shared by the reasons of open call sites (issue #393) and by
/// the count of unresolved sites naming a target (issue #392), so the two read
/// one spelling rule.
pub fn callee_names_type(callee: &str) -> Option<&str> {
    let segments = callee_path_segments(callee)?;
    segments.get(segments.len() - 2).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language_provider::provider_for;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Sym {
        qn: String,
        file: String,
    }

    fn sym(file: &str, qn: &str) -> Sym {
        Sym {
            qn: qn.to_string(),
            file: file.to_string(),
        }
    }

    fn run(
        candidates: &[Sym],
        imports_hint: &[String],
        caller_file: &str,
        lang: &str,
    ) -> Resolution<Sym> {
        let provider = provider_for(lang);
        let ev = CallEvidence {
            imports_hint,
            caller_file,
        };
        resolve_two_pass(
            candidates,
            |s| s.qn.clone(),
            |s| s.file.clone(),
            provider,
            &ev,
        )
    }

    #[test]
    fn strip_seq_suffix_removes_trailing_numeric_disambiguator() {
        assert_eq!(
            strip_seq_suffix("src/File.kt::process#7"),
            "src/File.kt::process"
        );
        assert_eq!(
            strip_seq_suffix("src/File.kt::Utils::process#12"),
            "src/File.kt::Utils::process"
        );
    }

    #[test]
    fn strip_seq_suffix_is_noop_without_numeric_tail() {
        assert_eq!(
            strip_seq_suffix("src/models.rs::Config::new"),
            "src/models.rs::Config::new"
        );
        assert_eq!(strip_seq_suffix("weird#not-a-number"), "weird#not-a-number");
    }

    #[test]
    fn kotlin_package_pass_resolves_cross_file_import_match() {
        // Two `process` candidates in different packages/directories; the
        // caller's own qualifier text names one of them uniquely.
        let candidates = vec![
            sym("pkg/a/File.kt", "pkg/a/File.kt::process#1"),
            sym("pkg/b/File.kt", "pkg/b/File.kt::process#2"),
        ];
        let imports = ["pkg.b.process".to_string()];
        let r = run(&candidates, &imports, "pkg/c/Caller.kt", "kotlin");
        match r {
            Resolution::Resolved {
                target, evidence, ..
            } => {
                assert_eq!(target.file, "pkg/b/File.kt");
                assert_eq!(evidence, ambiguity_policy::Evidence::ImportMatch);
            }
            other => panic!("expected Resolved via ImportMatch, got {other:?}"),
        }
    }

    #[test]
    fn kotlin_file_pass_resolves_package_proximity_without_import() {
        let candidates = vec![
            sym("pkg/a/Caller.kt", "pkg/a/Other.kt::helper#1"),
            sym("pkg/z/Unrelated.kt", "pkg/z/Unrelated.kt::helper#2"),
        ];
        let r = run(&candidates, &[], "pkg/a/Caller.kt", "kotlin");
        match r {
            Resolution::Resolved {
                target, evidence, ..
            } => {
                assert_eq!(target.file, "pkg/a/Caller.kt");
                assert_eq!(evidence, ambiguity_policy::Evidence::PackageProximity);
            }
            other => panic!("expected Resolved via PackageProximity, got {other:?}"),
        }
    }

    #[test]
    fn kotlin_genuinely_ambiguous_stays_ambiguous() {
        let candidates = vec![
            sym("pkg/a/A.kt", "pkg/a/A.kt::helper#1"),
            sym("pkg/z/Z.kt", "pkg/z/Z.kt::helper#2"),
        ];
        let r = run(&candidates, &[], "pkg/m/Caller.kt", "kotlin");
        match r {
            Resolution::Ambiguous { candidates } => assert_eq!(candidates.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn non_kotlin_language_matches_pre_issue_29_behavior() {
        // Rust has no `#seq` and no package_of override: the package pass
        // must degenerate to a no-op (identical bare-name key for every
        // candidate) and fall through to the file pass, which is exactly
        // the pre-issue-#29 single-pass mechanism.
        let candidates = vec![
            sym("src/models.rs", "src/models.rs::Config::new"),
            sym("src/other.rs", "src/other.rs::Widget::new"),
        ];
        let imports = ["Config::new".to_string()];
        let r = run(&candidates, &imports, "src/main.rs", "rust");
        match r {
            Resolution::Resolved {
                target, evidence, ..
            } => {
                assert_eq!(target.file, "src/models.rs");
                assert_eq!(evidence, ambiguity_policy::Evidence::ImportMatch);
            }
            other => panic!("expected Resolved via ImportMatch (file pass), got {other:?}"),
        }
    }

    #[test]
    fn callee_names_type_reads_the_qualifier_before_the_last_segment() {
        assert_eq!(callee_names_type("TaskSet::new"), Some("TaskSet"));
        assert_eq!(callee_names_type("crate::a::TaskSet::new"), Some("TaskSet"));
        assert_eq!(callee_names_type("io::BufWriter::new"), Some("BufWriter"));
        assert_eq!(callee_names_type("Vec::<u8>::with_capacity"), Some("Vec"));
        assert_eq!(callee_names_type("a::helper"), Some("a"));
        assert_eq!(callee_names_type("x.len"), None);
        assert_eq!(callee_names_type("helper"), None);
        assert_eq!(callee_names_type("(a + b).m"), None);
        assert_eq!(
            callee_path_segments("io::BufWriter::new"),
            Some(vec!["io", "BufWriter", "new"])
        );
    }
}
