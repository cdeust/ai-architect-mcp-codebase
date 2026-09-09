// resolver::receiver::local — palier 3: Rust local-variable receiver-call
// static binding (`s.<m>` where `s` is a once-bound-and-typed local).
//
// Split out of `receiver/mod.rs` (paliers 1-2's `self`/`Self` binding) to
// keep that file under the §4.1 500-line cap — the natural seam the
// coordinator named: tier-1/tier-2 self binding stays in `mod.rs`, this
// file carries the new tier-3 local binding it sits beside.
//
// source: tasks/plan-issues-282-283-284.md §2.2/§2.3 (lot 6) and
// ADR-<pending> (content in the lot-6 PR body — coordinator note
// 2026-09-09: wiki_adr unavailable this session).

use super::*;

/// Resolves a local-variable receiver call (`s.<m>`, palier 3) against the
/// parser-attached `receiver_hint` type, per plan §2.2 palier 3.
///
/// precondition: `hint` is the non-empty `CallSite.receiver_hint` the Rust
/// parser derived for this call (`rust_receiver::receiver_hint`) — the
/// caller (`resolver::calls::rust_local_receiver_gate`) does not invoke this
/// for an empty hint; `m` is the method name from `ReceiverForm::Local`;
/// `caller_file` is the calling symbol's own file id.
/// postcondition: `NotFound` when zero `idx.by_name[m]` candidates have a
/// parent type (last segment, generics stripped) matching `hint`;
/// `Resolved` (evidence `ReceiverLocalBinding`) when exactly one such
/// candidate exists, OR when 2+ exist but exactly one is defined in
/// `caller_file` (same-file preference, plan §2.2: "deux fichiers
/// définissant TaskSet::m ... → même fichier"); `Ambiguous` otherwise —
/// never a silent fall-through to a bare-name lookup, mirroring
/// `resolve_receiver_bound`'s zero-false-callers discipline.
pub(in crate::resolver) fn resolve_local_receiver_bound(
    idx: &SymbolIndex,
    hint: &str,
    m: &str,
    caller_file: &str,
) -> PolicyResolution<SymbolEntry> {
    let hint_last = strip_generics(last_segment(hint));
    let candidates: Vec<SymbolEntry> = idx
        .by_name
        .get(m)
        .into_iter()
        .flatten()
        .filter(|e| parent_last_segment_matches(&e.qualified_name, hint_last))
        .cloned()
        .collect();
    match candidates.len() {
        0 => PolicyResolution::NotFound,
        1 => local_receiver_bound(candidates.into_iter().next().expect("len == 1")),
        _ => match same_file_candidate(&candidates, caller_file) {
            Some(target) => local_receiver_bound(target),
            None => PolicyResolution::Ambiguous { candidates },
        },
    }
}

/// `Some(entry)` iff EXACTLY ONE of `candidates` is defined in `caller_file`
/// — the palier-3 same-file tiebreak (plan §2.2), distinct from palier 1-2's
/// exact-key/parent-match rule: a local's declared type narrows the
/// candidate set to a NAME+TYPE pair that can still span 2+ files (two
/// crates/modules defining a homonymous type+method), so this tier gets one
/// more, narrower, tiebreak before giving up as `Ambiguous`.
fn same_file_candidate(candidates: &[SymbolEntry], caller_file: &str) -> Option<SymbolEntry> {
    let mut same_file = candidates
        .iter()
        .filter(|e| extract_file_prefix_or_self(&e.qualified_name) == caller_file);
    let first = same_file.next()?;
    if same_file.next().is_some() {
        return None;
    }
    Some(first.clone())
}

fn local_receiver_bound(target: SymbolEntry) -> PolicyResolution<SymbolEntry> {
    PolicyResolution::Resolved {
        target,
        evidence: ambiguity_policy::Evidence::ReceiverLocalBinding,
        confidence: ambiguity_policy::confidence_for(
            ambiguity_policy::Evidence::ReceiverLocalBinding,
        ),
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

    #[test]
    fn palier3_unique_candidate_under_hinted_type_resolves() {
        let target = entry("id1", "Method", "src/lib.rs::TaskSet::m");
        let idx = index_with(vec![], vec![target.clone()]);
        let res = resolve_local_receiver_bound(&idx, "TaskSet", "m", "tests/x.rs");
        match res {
            PolicyResolution::Resolved {
                target: t,
                evidence,
                confidence,
            } => {
                assert_eq!(t.id, target.id);
                assert_eq!(evidence, ambiguity_policy::Evidence::ReceiverLocalBinding);
                assert_eq!(
                    confidence,
                    ambiguity_policy::confidence_for(
                        ambiguity_policy::Evidence::ReceiverLocalBinding
                    )
                );
            }
            other => panic!("expected Resolved via palier 3, got {other:?}"),
        }
    }

    #[test]
    fn palier3_strips_generics_and_reduces_to_last_segment_of_the_hint() {
        let target = entry("id1", "Method", "src/lib.rs::helpers::Wrapper::get");
        let idx = index_with(vec![], vec![target.clone()]);
        let res = resolve_local_receiver_bound(&idx, "Wrapper<u8>", "get", "caller.rs");
        assert!(matches!(res, PolicyResolution::Resolved { .. }));
    }

    #[test]
    fn palier3_two_files_defining_the_hinted_method_prefers_the_callers_own_file() {
        let a = entry("a", "Method", "a.rs::TaskSet::m");
        let b = entry("b", "Method", "b.rs::TaskSet::m");
        let idx = index_with(vec![], vec![a.clone(), b]);
        let res = resolve_local_receiver_bound(&idx, "TaskSet", "m", "a.rs");
        match res {
            PolicyResolution::Resolved { target: t, .. } => assert_eq!(t.id, a.id),
            other => panic!("expected Resolved via same-file preference, got {other:?}"),
        }
    }

    #[test]
    fn palier3_caller_in_neither_file_is_ambiguous() {
        let a = entry("a", "Method", "a.rs::TaskSet::m");
        let b = entry("b", "Method", "b.rs::TaskSet::m");
        let idx = index_with(vec![], vec![a, b]);
        let res = resolve_local_receiver_bound(&idx, "TaskSet", "m", "c.rs");
        match res {
            PolicyResolution::Ambiguous { candidates } => assert_eq!(candidates.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn palier3_missing_method_is_not_found_never_falls_back_to_bare_name() {
        let idx = index_with(vec![], vec![]);
        let res = resolve_local_receiver_bound(&idx, "TaskSet", "missing", "src/lib.rs");
        assert_eq!(res, PolicyResolution::NotFound);
    }
}
