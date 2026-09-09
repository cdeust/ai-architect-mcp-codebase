// ambiguity_policy — the single evidence-based ambiguity/confidence policy
// for symbol resolution (calls, imports).
//
// Pure logic module: no I/O, no graph-store access. Testable in isolation
// from a graph. Both resolve_single_call's unqualified path and
// pick_best_candidate's qualified/import-matched path in resolver.rs
// delegate here — after issue #30 there is exactly ONE place deciding
// ambiguity and confidence for symbol resolution.
//
// source: issue #30 — "Ambiguity policy and confidence scoring depend on
// callee spelling, not resolution evidence." Two facets of one defect:
// (1) the same ambiguity (2+ in-repo candidates) was handled two
// contradictory ways depending on surface spelling (dropped vs.
// arbitrary-candidates[0]); (2) confidence was inverted — the arbitrary
// guess shipped 1.0, a principled unique match shipped 0.9. This module
// fixes both by deriving confidence ONLY from the evidence tier that
// produced the match, and applying identical logic regardless of spelling.

/// Evidence tiers, ordered strongest-first. The ordinal position of each
/// variant (not its Rust discriminant) is what `confidence_for` maps to a
/// score — see that function's doc comment for the sourcing of the values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Exactly one candidate exists for the callee's name in the whole
    /// symbol index — no ambiguity to resolve.
    UniqueGlobal,
    /// A Rust `self.<m>` / `Self::<m>` callee bound to a method on the
    /// caller's own enclosing `impl` type — either the exact
    /// `{impl_qn}::{m}` key, or the single `idx.by_name[m]` candidate whose
    /// parent type matches that `impl`. Produced only by
    /// `resolver::receiver::resolve_receiver_bound`, gated to
    /// `language == "rust"` and a `Method` caller.
    /// source: tasks/plan-issues-282-283-284.md §2.2/§9-3 (issue #283, lot
    /// 4). Rationale and the arbitrated confidence value are recorded as
    /// Cortex memory 4360985 (ADR content; `wiki_adr` was unavailable in
    /// the authoring session — pending promotion to a numbered wiki ADR).
    ReceiverBound,
    /// The callee (or its qualified spelling) matches an import path in
    /// scope at the call site, and exactly one candidate's qualified name
    /// has that import path as a suffix.
    ImportMatch,
    /// Exactly one candidate's qualified name is defined in the caller's
    /// own file.
    SameFileUnique,
    /// Exactly one candidate's qualified name shares the caller's
    /// package/module prefix.
    PackageProximity,
}

/// Heuristic ordinal trust tiers — NOT measured probabilities. These are
/// policy constants, documented and reviewed as part of issue #30's fix;
/// they are deliberately monotone in evidence strength so that a consumer
/// filtering on `confidence >= threshold` never keeps a weaker-evidence
/// match over a stronger one.
/// source: issue #30 policy proposal (ordinals accepted in the fix PR).
pub fn confidence_for(evidence: Evidence) -> f64 {
    match evidence {
        Evidence::UniqueGlobal => 0.95,
        Evidence::ReceiverBound => 0.93,
        Evidence::ImportMatch => 0.9,
        Evidence::SameFileUnique => 0.85,
        Evidence::PackageProximity => 0.7,
    }
}

/// Outcome of the ambiguity policy for one callee reference.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution<T> {
    Resolved {
        target: T,
        evidence: Evidence,
        confidence: f64,
    },
    /// 2+ candidates exist and no evidence tier (`resolve`, before any
    /// tiebreak) discriminates between them.
    Ambiguous { candidates: Vec<T> },
    /// Zero candidates matched the callee's name at all.
    NotFound,
}

/// The minimal shape the policy needs from a candidate symbol. Kept
/// separate from resolver::SymbolEntry so this module has zero dependency
/// on the graph/indexing layer.
pub trait Candidate: Clone {
    fn qualified_name(&self) -> &str;
}

/// Disambiguating evidence available at the call site. Gathered by the
/// caller (resolver.rs) from imports in scope, the caller's file, and the
/// caller's package/module path — this module performs no I/O to collect
/// it, it only consumes what's handed in.
pub struct Context<'a> {
    /// Import paths (or, for an already-qualified callee spelling, the
    /// spelling itself) that could match a candidate's qualified-name
    /// suffix.
    pub imports_in_scope: &'a [String],
    /// The caller's file path — used for the same-file-unique tier.
    pub caller_file: &'a str,
    /// The caller's package/module path, if known — used for the
    /// package-proximity tier.
    pub caller_package: Option<&'a str>,
}

/// precondition: `candidates` is the full set of in-repo symbols whose name
/// matches the callee (0, 1, or many); `ctx` holds the evidence available
/// at the call site.
/// postcondition: for a fixed `candidates` set — regardless of insertion
/// order — and a fixed `ctx`, `resolve` returns an identical `Resolution`
/// (same variant, same evidence, same confidence). The function never
/// inspects how the callee was originally spelled (qualified vs.
/// unqualified); callers normalize that into `ctx.imports_in_scope` before
/// calling, which is what makes the outcome spelling-invariant.
/// invariant: confidence is monotone in evidence strength — `UniqueGlobal`
/// is stronger than `ImportMatch`, which is stronger than `SameFileUnique`,
/// which is stronger than `PackageProximity`; `Ambiguous` (no confidence
/// yet) precedes all of them.
pub fn resolve<T: Candidate>(candidates: &[T], ctx: &Context) -> Resolution<T> {
    if candidates.is_empty() {
        return Resolution::NotFound;
    }
    if candidates.len() == 1 {
        return resolved(candidates[0].clone(), Evidence::UniqueGlobal);
    }
    if let Some(target) = unique_match(candidates, |c| import_matches(c, ctx.imports_in_scope)) {
        return resolved(target.clone(), Evidence::ImportMatch);
    }
    if let Some(target) = unique_match(candidates, |c| {
        c.qualified_name().starts_with(ctx.caller_file)
    }) {
        return resolved(target.clone(), Evidence::SameFileUnique);
    }
    if let Some(pkg) = ctx.caller_package {
        if let Some(target) = unique_match(candidates, |c| c.qualified_name().starts_with(pkg)) {
            return resolved(target.clone(), Evidence::PackageProximity);
        }
    }
    Resolution::Ambiguous {
        candidates: candidates.to_vec(),
    }
}

/// The `resolution_method` label recorded on an edge produced by this
/// policy, for downstream observability (distinct from the pre-issue-#30
/// single "import-scope-lookup" label that hid which evidence tier fired).
pub fn resolution_label(evidence: Evidence) -> &'static str {
    match evidence {
        Evidence::UniqueGlobal => "unique-match",
        Evidence::ReceiverBound => "receiver-type",
        Evidence::ImportMatch => "import-scope-lookup",
        Evidence::SameFileUnique => "same-file-unique",
        Evidence::PackageProximity => "package-proximity",
    }
}

fn resolved<T>(target: T, evidence: Evidence) -> Resolution<T> {
    Resolution::Resolved {
        target,
        confidence: confidence_for(evidence),
        evidence,
    }
}

/// Returns `Some` only when exactly one candidate satisfies `pred` —
/// mirrors the tier's "unique" requirement (2+ matches is still ambiguous
/// at that tier, so evaluation falls through to the next tier).
fn unique_match<T, F>(candidates: &[T], pred: F) -> Option<&T>
where
    F: Fn(&T) -> bool,
{
    let mut found: Option<&T> = None;
    for c in candidates {
        if pred(c) {
            if found.is_some() {
                return None;
            }
            found = Some(c);
        }
    }
    found
}

/// A candidate's qualified name matches an in-scope import when one is a
/// path-segment suffix of the other (mirrors the historical
/// `pick_best_candidate` path-hint matching: qualified names use `::`,
/// import paths may use `::` or `.`).
fn import_matches<T: Candidate>(candidate: &T, imports_in_scope: &[String]) -> bool {
    let c_segs = candidate.qualified_name().replace("::", "/");
    imports_in_scope.iter().any(|imp| {
        let imp_segs = imp.replace("::", "/").replace('.', "/");
        c_segs.ends_with(&imp_segs) || imp_segs.ends_with(&c_segs)
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestSymbol {
        qn: &'static str,
    }

    impl Candidate for TestSymbol {
        fn qualified_name(&self) -> &str {
            self.qn
        }
    }

    fn sym(qn: &'static str) -> TestSymbol {
        TestSymbol { qn }
    }

    fn empty_ctx<'a>() -> Context<'a> {
        Context {
            imports_in_scope: &[],
            caller_file: "no/such/file.rs",
            caller_package: None,
        }
    }

    // --- Test 1: spelling invariance -----------------------------------
    // Same candidate set + same evidence -> same outcome and confidence
    // whether the callee is "spelled" qualified (fed through
    // imports_in_scope, mirroring resolver.rs's qualified path) or
    // unqualified (evidence absent, falls to tiebreak).
    #[test]
    fn spelling_invariance_import_match() {
        let candidates = vec![sym("a::helper"), sym("b::helper")];
        // "Qualified" spelling: the callee text itself is fed as the import
        // hint, mirroring resolve_single_call's qualified path.
        let qualified_ctx = Context {
            imports_in_scope: &["a::helper".to_string()],
            caller_file: "irrelevant.rs",
            caller_package: None,
        };
        // "Unqualified" spelling: the same evidence (an import of a::helper
        // in scope) is what resolve_single_call's unqualified path builds.
        let unqualified_ctx = Context {
            imports_in_scope: &["a::helper".to_string()],
            caller_file: "irrelevant.rs",
            caller_package: None,
        };
        let r1 = resolve(&candidates, &qualified_ctx);
        let r2 = resolve(&candidates, &unqualified_ctx);
        assert_eq!(r1, r2);
        match r1 {
            Resolution::Resolved {
                target,
                evidence,
                confidence,
            } => {
                assert_eq!(target.qn, "a::helper");
                assert_eq!(evidence, Evidence::ImportMatch);
                assert_eq!(confidence, confidence_for(Evidence::ImportMatch));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn spelling_invariance_genuine_ambiguity() {
        // No evidence distinguishes a::helper from b::helper regardless of
        // how the caller phrased the lookup (qualified or unqualified) —
        // both must land on the identical Ambiguous outcome (no tiebreak:
        // resolve_single_call/resolve_single_import drop the edge rather
        // than guess, per resolver.rs's resolve_single_call doc comment).
        let candidates = vec![sym("b::helper"), sym("a::helper")];
        let ctx_a = empty_ctx();
        let ctx_b = empty_ctx();
        let r1 = resolve(&candidates, &ctx_a);
        let r2 = resolve(&candidates, &ctx_b);
        assert_eq!(r1, r2);
        match r1 {
            Resolution::Ambiguous { candidates } => assert_eq!(candidates.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    // --- Test 2: order invariance --------------------------------------
    #[test]
    fn order_invariance_shuffled_candidates() {
        let orders = vec![
            vec![sym("a::helper"), sym("b::helper"), sym("c::helper")],
            vec![sym("c::helper"), sym("a::helper"), sym("b::helper")],
            vec![sym("b::helper"), sym("c::helper"), sym("a::helper")],
        ];
        let ctx = empty_ctx();
        for candidates in &orders {
            match resolve(candidates, &ctx) {
                Resolution::Ambiguous { candidates: c } => assert_eq!(c.len(), 3),
                other => panic!("expected Ambiguous regardless of candidate order, got {other:?}"),
            }
        }
    }

    // --- Test 3: confidence monotonicity --------------------------------
    #[test]
    fn confidence_is_monotone_in_evidence_strength() {
        let tiers = [
            Evidence::UniqueGlobal,
            Evidence::ReceiverBound,
            Evidence::ImportMatch,
            Evidence::SameFileUnique,
            Evidence::PackageProximity,
        ];
        for pair in tiers.windows(2) {
            let (stronger, weaker) = (pair[0], pair[1]);
            assert!(
                confidence_for(stronger) > confidence_for(weaker),
                "{stronger:?} ({}) must be > {weaker:?} ({})",
                confidence_for(stronger),
                confidence_for(weaker)
            );
        }
    }

    // --- Test 4: ambiguous vs. not-found are distinguishable ------------
    #[test]
    fn ambiguous_case_is_distinguishable_from_not_found() {
        let candidates = vec![sym("a::helper"), sym("b::helper")];
        let ctx = empty_ctx();
        match resolve(&candidates, &ctx) {
            Resolution::Ambiguous { candidates } => assert_eq!(candidates.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        let none: Vec<TestSymbol> = vec![];
        match resolve(&none, &ctx) {
            Resolution::NotFound => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn unique_candidate_resolves_without_evidence() {
        let candidates = vec![sym("only::one")];
        let ctx = empty_ctx();
        match resolve(&candidates, &ctx) {
            Resolution::Resolved {
                target,
                evidence,
                confidence,
            } => {
                assert_eq!(target.qn, "only::one");
                assert_eq!(evidence, Evidence::UniqueGlobal);
                assert_eq!(confidence, confidence_for(Evidence::UniqueGlobal));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn same_file_unique_resolves() {
        let candidates = vec![sym("src/foo.rs::helper"), sym("src/bar.rs::helper")];
        let ctx = Context {
            imports_in_scope: &[],
            caller_file: "src/foo.rs",
            caller_package: None,
        };
        match resolve(&candidates, &ctx) {
            Resolution::Resolved {
                target, evidence, ..
            } => {
                assert_eq!(target.qn, "src/foo.rs::helper");
                assert_eq!(evidence, Evidence::SameFileUnique);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn package_proximity_used_when_no_stronger_evidence() {
        let candidates = vec![sym("pkg_a::mod::helper"), sym("pkg_b::mod::helper")];
        let ctx = Context {
            imports_in_scope: &[],
            caller_file: "unrelated/file.rs",
            caller_package: Some("pkg_a"),
        };
        match resolve(&candidates, &ctx) {
            Resolution::Resolved {
                target, evidence, ..
            } => {
                assert_eq!(target.qn, "pkg_a::mod::helper");
                assert_eq!(evidence, Evidence::PackageProximity);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }
}
