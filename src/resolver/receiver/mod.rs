// resolver::receiver — Rust `self`/`Self`/local-variable receiver-call
// static binding.
//
// source: tasks/plan-issues-282-283-284.md §2.2/§2.3 (lots 4 and 6) and
// ADR-9840 (paliers 1-2) / ADR-<pending> (palier 3, content in the lot-6 PR
// body — coordinator note 2026-09-09: wiki_adr unavailable this session).
//
// Paliers 1-2 (`self.<m>` / `Self::<m>` on a `Method` caller,
// `resolve_receiver_bound`, this file) and palier 3 (`s.<m>` where `s` is a
// once-bound-and-typed local, ANY caller, `resolve_local_receiver_bound`,
// the `local` submodule — split out to keep this file under the §4.1
// 500-line cap) are two independent gates in `resolver::calls`: palier 3
// does not require a `Method` caller (a free function's local variable
// qualifies exactly as well as a method's), and it consumes the
// parser-attached `CallSite.receiver_hint` rather than the caller's own
// enclosing `impl`.

use super::*;

mod local;
pub(super) use local::resolve_local_receiver_bound;

/// The receiver shape of a callee as spelled at the call site. `classify`
/// never inspects surrounding context (types, scope) — only the callee
/// text itself; it is spelling analysis, not resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReceiverForm {
    /// `self.<m>` — value receiver.
    SelfValue(String),
    /// `Self::<m>` — type-relative associated call.
    SelfType(String),
    /// `<ident>.<m>` where `ident` is a plain identifier (not `self`) —
    /// resolved by palier 3 (lot 6) via the parser-attached `receiver_hint`
    /// keyed on `ident`'s bound type, not on `ident` itself (two locals of
    /// the same type at different call sites share no state here).
    Local { ident: String, m: String },
    /// Anything else: chained calls, index expressions, tuple/field
    /// access chains, or an already-qualified (`::`) path.
    None,
}

/// precondition: `callee` is the raw callee spelling `call_callee` parsed
/// (e.g. `self.response_of`, `Self::new`, `sets[0].response_of`).
/// postcondition: returns the receiver form implied by that spelling alone.
/// A malformed or chained receiver (anything containing `(`, `[`, or a
/// second `.` in the member position) always classifies as `None` — the
/// classifier never guesses at a receiver it cannot parse outright.
pub(super) fn classify(callee: &str) -> ReceiverForm {
    if let Some(m) = callee.strip_prefix("self.") {
        return if is_plain_ident(m) {
            ReceiverForm::SelfValue(m.to_string())
        } else {
            ReceiverForm::None
        };
    }
    if let Some(m) = callee.strip_prefix("Self::") {
        return if is_plain_ident(m) {
            ReceiverForm::SelfType(m.to_string())
        } else {
            ReceiverForm::None
        };
    }
    if let Some((ident, m)) = callee.split_once('.') {
        if is_plain_ident(ident) && is_plain_ident(m) {
            return ReceiverForm::Local {
                ident: ident.to_string(),
                m: m.to_string(),
            };
        }
    }
    ReceiverForm::None
}

/// A bare identifier: ASCII letter/underscore start, ASCII
/// alphanumeric/underscore rest, non-empty. Rejects anything a receiver
/// classifier must NOT guess through — `(`, `[`, a further `.`, generics.
fn is_plain_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// precondition: `caller_qn` is a `Method`'s qualified name (e.g.
/// `src/lib.rs::TaskSet::total`).
/// postcondition: `Some(impl_qn)` is everything before the last `::`
/// segment (the enclosing `impl`'s type QN); `None` when `caller_qn` has no
/// `::` separator (never happens for an indexed `Method`, but this stays
/// total rather than panicking on an unexpected caller shape).
pub(super) fn impl_qn_of(caller_qn: &str) -> Option<&str> {
    caller_qn.rsplit_once("::").map(|(impl_qn, _)| impl_qn)
}

/// Strips a trailing generic-parameter list: `Wrapper<T>` -> `Wrapper`.
/// postcondition: returns `s` unchanged when it has no `<`.
pub(super) fn strip_generics(s: &str) -> &str {
    match s.find('<') {
        Some(i) => &s[..i],
        None => s,
    }
}

/// Resolves a `self`/`Self` receiver call against the enclosing `impl`'s
/// type, per plan §2.2 paliers 1-2.
///
/// precondition: `form` is the callee's classified receiver shape;
/// `impl_qn` is the caller's enclosing impl-type QN (`impl_qn_of` on the
/// caller), when known.
/// postcondition: `None` when `form` is not `SelfValue`/`SelfType`, or when
/// `impl_qn` is `None` — the caller must fall back to the pre-existing
/// by-name resolution path in both cases (this function makes no claim
/// about those callees at all). Otherwise returns `Some(Resolved)` (palier
/// 1: exact `{impl_qn}::{m}` key, or palier 2: exactly one `idx.by_name[m]`
/// candidate whose parent type matches `impl_qn`'s last segment),
/// `Some(Ambiguous)` (2+ palier-2 candidates), or `Some(NotFound)` (zero
/// candidates) — never a silent fall-through to a bare-name lookup, which
/// is what would reintroduce false callers (plan §2.2, "jamais de repli sur
/// le nom seul").
pub(super) fn resolve_receiver_bound(
    idx: &SymbolIndex,
    form: &ReceiverForm,
    impl_qn: Option<&str>,
) -> Option<PolicyResolution<SymbolEntry>> {
    let m = match form {
        ReceiverForm::SelfValue(m) | ReceiverForm::SelfType(m) => m,
        ReceiverForm::Local { .. } | ReceiverForm::None => return None,
    };
    let impl_qn = impl_qn?;

    // Palier 1: exact qualified-name key.
    let key = format!("{impl_qn}::{m}");
    if let Some(entry) = idx.by_qn.get(&key) {
        return Some(receiver_bound(entry.clone()));
    }

    // Palier 2: unique candidate whose parent type (last segment, generics
    // stripped) equals impl_qn's own last segment (also stripped).
    let impl_last = strip_generics(last_segment(impl_qn));
    let candidates: Vec<SymbolEntry> = idx
        .by_name
        .get(m.as_str())
        .into_iter()
        .flatten()
        .filter(|e| parent_last_segment_matches(&e.qualified_name, impl_last))
        .cloned()
        .collect();

    Some(match candidates.len() {
        0 => PolicyResolution::NotFound,
        1 => receiver_bound(candidates.into_iter().next().expect("len == 1")),
        _ => PolicyResolution::Ambiguous { candidates },
    })
}

fn last_segment(qn: &str) -> &str {
    qn.rsplit("::").next().unwrap_or(qn)
}

/// True when `candidate_qn`'s parent (everything before its own last `::`
/// segment) ends, at its own last segment with generics stripped, in
/// `impl_last`.
fn parent_last_segment_matches(candidate_qn: &str, impl_last: &str) -> bool {
    match candidate_qn.rsplit_once("::") {
        Some((parent, _method)) => strip_generics(last_segment(parent)) == impl_last,
        None => false,
    }
}

fn receiver_bound(target: SymbolEntry) -> PolicyResolution<SymbolEntry> {
    PolicyResolution::Resolved {
        target,
        evidence: ambiguity_policy::Evidence::ReceiverBound,
        confidence: ambiguity_policy::confidence_for(ambiguity_policy::Evidence::ReceiverBound),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, label: &str, qn: &str) -> SymbolEntry {
        SymbolEntry {
            id: id.to_string(),
            label: label.to_string(),
            qualified_name: qn.to_string(),
        }
    }

    fn index_with(by_qn: Vec<SymbolEntry>, by_name: Vec<SymbolEntry>) -> SymbolIndex {
        let mut qn_map = HashMap::new();
        for e in by_qn {
            qn_map.insert(e.qualified_name.clone(), e);
        }
        let mut name_map: HashMap<String, Vec<SymbolEntry>> = HashMap::new();
        for e in by_name {
            let name = last_segment(&e.qualified_name).to_string();
            name_map.entry(name).or_default().push(e);
        }
        SymbolIndex {
            by_name: name_map,
            by_qn: qn_map,
            by_parent_module: HashMap::new(),
        }
    }

    // --- classify ---------------------------------------------------------

    #[test]
    fn classify_self_value() {
        assert_eq!(
            classify("self.response_of"),
            ReceiverForm::SelfValue("response_of".to_string())
        );
    }

    #[test]
    fn classify_self_type() {
        assert_eq!(
            classify("Self::new"),
            ReceiverForm::SelfType("new".to_string())
        );
    }

    #[test]
    fn classify_local_binding_candidate() {
        assert_eq!(
            classify("trial.response_of"),
            ReceiverForm::Local {
                ident: "trial".to_string(),
                m: "response_of".to_string(),
            }
        );
    }

    #[test]
    fn classify_chained_self_call_is_none() {
        // self.tasks.get — a second `.` in the member position: not a
        // plain method name, must not be guessed at.
        assert_eq!(classify("self.tasks.get"), ReceiverForm::None);
    }

    #[test]
    fn classify_index_expression_receiver_is_none() {
        assert_eq!(classify("sets[0].response_of"), ReceiverForm::None);
    }

    #[test]
    fn classify_chained_method_call_receiver_is_none() {
        assert_eq!(classify("x.trim().len"), ReceiverForm::None);
    }

    // --- impl_qn_of / strip_generics --------------------------------------

    #[test]
    fn impl_qn_of_strips_last_segment() {
        assert_eq!(
            impl_qn_of("src/lib.rs::TaskSet::total"),
            Some("src/lib.rs::TaskSet")
        );
    }

    #[test]
    fn impl_qn_of_none_without_separator() {
        assert_eq!(impl_qn_of("total"), None);
    }

    #[test]
    fn strip_generics_removes_trailing_params() {
        assert_eq!(strip_generics("Wrapper<T>"), "Wrapper");
        assert_eq!(strip_generics("TaskSet"), "TaskSet");
    }

    // --- resolve_receiver_bound: palier 1 ----------------------------------

    #[test]
    fn palier1_exact_key_resolves() {
        let target = entry("id1", "Method", "src/lib.rs::TaskSet::response_of");
        let idx = index_with(vec![target.clone()], vec![]);
        let form = ReceiverForm::SelfValue("response_of".to_string());
        let res = resolve_receiver_bound(&idx, &form, Some("src/lib.rs::TaskSet"));
        match res {
            Some(PolicyResolution::Resolved {
                target: t,
                evidence,
                confidence,
            }) => {
                assert_eq!(t.qualified_name, "src/lib.rs::TaskSet::response_of");
                assert_eq!(evidence, ambiguity_policy::Evidence::ReceiverBound);
                assert_eq!(
                    confidence,
                    ambiguity_policy::confidence_for(ambiguity_policy::Evidence::ReceiverBound)
                );
            }
            other => panic!("expected Resolved via palier 1, got {other:?}"),
        }
    }

    #[test]
    fn palier1_self_type_resolves_associated_call() {
        let target = entry("id1", "Method", "src/lib.rs::TaskSet::new");
        let idx = index_with(vec![target], vec![]);
        let form = ReceiverForm::SelfType("new".to_string());
        let res = resolve_receiver_bound(&idx, &form, Some("src/lib.rs::TaskSet"));
        assert!(matches!(res, Some(PolicyResolution::Resolved { .. })));
    }

    // --- resolve_receiver_bound: palier 2 ----------------------------------

    #[test]
    fn palier2_unique_candidate_under_named_type_resolves() {
        // No exact key (different file), but exactly one `m` candidate
        // whose parent's last segment equals impl_qn's last segment.
        let candidate = entry("id1", "Method", "other/file.rs::TaskSet::response_of");
        let idx = index_with(vec![], vec![candidate.clone()]);
        let form = ReceiverForm::SelfValue("response_of".to_string());
        let res = resolve_receiver_bound(&idx, &form, Some("src/lib.rs::TaskSet"));
        match res {
            Some(PolicyResolution::Resolved { target: t, .. }) => {
                assert_eq!(t.id, candidate.id);
            }
            other => panic!("expected Resolved via palier 2, got {other:?}"),
        }
    }

    #[test]
    fn palier2_two_homonymous_types_are_ambiguous() {
        let a = entry("a", "Method", "a.rs::TaskSet::m");
        let b = entry("b", "Method", "b.rs::TaskSet::m");
        let idx = index_with(vec![], vec![a, b]);
        let form = ReceiverForm::SelfValue("m".to_string());
        // Caller lives in neither a.rs nor b.rs — no exact key, and two
        // parent-matching candidates: must not guess.
        let res = resolve_receiver_bound(&idx, &form, Some("c.rs::TaskSet"));
        match res {
            Some(PolicyResolution::Ambiguous { candidates }) => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn missing_method_is_not_found_never_falls_back_to_bare_name() {
        let idx = index_with(vec![], vec![]);
        let form = ReceiverForm::SelfValue("missing".to_string());
        let res = resolve_receiver_bound(&idx, &form, Some("src/lib.rs::TaskSet"));
        assert_eq!(res, Some(PolicyResolution::NotFound));
    }

    // --- not applicable: caller must fall back ------------------------------

    #[test]
    fn local_form_is_not_handled_by_this_palier() {
        let idx = index_with(vec![], vec![]);
        let form = ReceiverForm::Local {
            ident: "s".to_string(),
            m: "response_of".to_string(),
        };
        assert_eq!(
            resolve_receiver_bound(&idx, &form, Some("src/lib.rs::TaskSet")),
            None
        );
    }

    #[test]
    fn none_form_is_not_handled_by_this_palier() {
        let idx = index_with(vec![], vec![]);
        assert_eq!(
            resolve_receiver_bound(&idx, &ReceiverForm::None, Some("src/lib.rs::TaskSet")),
            None
        );
    }

    #[test]
    fn missing_impl_qn_is_not_handled_by_this_palier() {
        let idx = index_with(vec![], vec![]);
        let form = ReceiverForm::SelfValue("response_of".to_string());
        assert_eq!(resolve_receiver_bound(&idx, &form, None), None);
    }

    // `resolve_local_receiver_bound` (palier 3) tests live in the `local`
    // submodule alongside the function they exercise — split out to keep
    // this file under the §4.1 500-line cap.
}
