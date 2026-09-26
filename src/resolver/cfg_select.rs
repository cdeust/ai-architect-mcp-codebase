// resolver::cfg_select: picks, among the twins of one item, the one a build
// compiles, when the build decides it (issue #353, part B).
//
// `cfg_twins` leaves a call to a twin set open, because the twins are distinct
// nodes and nothing separates them. Two facts can: the caller's own gate and the
// default build profile, both read by the verdict `cfg_verdict` shares with the
// language-server pass (issue #366).
//
// A twin is chosen only when exactly one twin is not ruled out AND that twin is
// shown compiled. A lone survivor that is merely undecided is not chosen: two
// twins are not guaranteed to be complementary, and a call must not be pinned
// to a twin the build might not compile. Every case that is not decided leaves
// the site open with the reason `cfg_twins`, as before.

use super::cfg_verdict::{caller_gates, verdict, Verdict};
use super::SymbolEntry;

pub(super) use super::cfg_verdict::TwinView;

/// The twin a call to `candidates` reaches, when every candidate is a twin of
/// one item and the build decides which one it compiles; `None` otherwise (an
/// ordinary ambiguity, or a twin set the build leaves open).
pub(super) fn choose<'a>(
    view: &TwinView,
    caller_qn: &str,
    candidates: &'a [SymbolEntry],
) -> Option<&'a SymbolEntry> {
    super::cfg_twins::are_twins_of_one_item(view, candidates)
        .then(|| select(view, caller_qn, candidates))
        .flatten()
}

/// The one twin among `twins` that the build compiles, or `None` when the
/// build does not decide it. `twins` are the candidates of one call, already
/// known to be twins of one item (`cfg_twins::are_twins_of_one_item`).
pub(super) fn select<'a>(
    view: &TwinView,
    caller_qn: &str,
    twins: &'a [SymbolEntry],
) -> Option<&'a SymbolEntry> {
    let caller = caller_gates(view, caller_qn);
    let mut not_ruled_out = twins
        .iter()
        .map(|twin| (twin, verdict(view, &caller, &twin.id, &twin.qualified_name)))
        .filter(|(_, v)| *v != Verdict::NotCompiled);
    let (twin, verdict) = not_ruled_out.next()?;
    if not_ruled_out.next().is_some() {
        return None;
    }
    (verdict == Verdict::Compiled).then_some(twin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn twin(qn: &str) -> SymbolEntry {
        SymbolEntry {
            id: qn.to_string(),
            label: "Function".to_string(),
            qualified_name: qn.to_string(),
        }
    }

    const FAST: &str = "src/lib.rs::pick#cfg(feature=fast)";
    const SLOW: &str = "src/lib.rs::pick#cfg(not(feature=fast))";
    const CALLER: &str = "src/lib.rs::caller";

    fn chosen<'a>(view: &TwinView, caller: &str, twins: &'a [SymbolEntry]) -> Option<&'a str> {
        select(view, caller, twins).map(|t| t.id.as_str())
    }

    #[test]
    fn the_twin_the_default_features_compile_is_chosen() {
        let twins = [twin(FAST), twin(SLOW)];
        let view = TwinView::with(&[(FAST, "inactive"), (SLOW, "active")]);
        assert_eq!(chosen(&view, CALLER, &twins), Some(SLOW));
        let flipped = TwinView::with(&[(FAST, "active"), (SLOW, "inactive")]);
        assert_eq!(chosen(&flipped, CALLER, &twins), Some(FAST));
    }

    #[test]
    fn nothing_is_chosen_when_the_profile_decides_neither_or_only_rules_one_out() {
        let twins = [twin(FAST), twin(SLOW)];
        assert_eq!(chosen(&TwinView::default(), CALLER, &twins), None);
        let unknown = TwinView::with(&[(FAST, "unknown"), (SLOW, "unknown")]);
        assert_eq!(chosen(&unknown, CALLER, &twins), None);
        // One twin is ruled out, the other is merely undecided: not chosen.
        let half = TwinView::with(&[(FAST, "inactive"), (SLOW, "unknown")]);
        assert_eq!(chosen(&half, CALLER, &twins), None);
    }

    #[test]
    fn two_twins_shown_compiled_are_not_told_apart() {
        let twins = [twin(FAST), twin(SLOW)];
        let both = TwinView::with(&[(FAST, "active"), (SLOW, "active")]);
        assert_eq!(chosen(&both, CALLER, &twins), None);
    }

    #[test]
    fn a_caller_under_the_gate_of_a_twin_reaches_that_twin_whatever_the_profile() {
        let helpers = [
            twin("src/lib.rs::helper#cfg(unix)"),
            twin("src/lib.rs::helper#cfg(not(unix))"),
        ];
        let in_unix = "src/lib.rs::run#cfg(unix)";
        assert_eq!(
            chosen(&TwinView::default(), in_unix, &helpers),
            Some("src/lib.rs::helper#cfg(unix)")
        );
        let in_other = "src/lib.rs::run#cfg(not(unix))";
        assert_eq!(
            chosen(&TwinView::default(), in_other, &helpers),
            Some("src/lib.rs::helper#cfg(not(unix))")
        );
    }

    #[test]
    fn the_callers_gate_outranks_the_profile() {
        let twins = [twin(FAST), twin(SLOW)];
        let view = TwinView::with(&[(FAST, "inactive"), (SLOW, "active")]);
        let in_fast = "src/lib.rs::run#cfg(feature=fast)";
        assert_eq!(chosen(&view, in_fast, &twins), Some(FAST));
    }

    #[test]
    fn a_caller_in_a_twin_module_carries_the_gate_of_the_module() {
        let helpers = [
            twin("src/lib.rs::helper#cfg(kani)"),
            twin("src/lib.rs::helper#cfg(not(kani))"),
        ];
        let in_module = "src/lib.rs::proofs#cfg(kani)::check";
        assert_eq!(
            chosen(&TwinView::default(), in_module, &helpers),
            Some("src/lib.rs::helper#cfg(kani)")
        );
    }

    #[test]
    fn a_caller_with_a_wider_gate_still_implies_a_twin_it_contains() {
        let helpers = [
            twin("src/lib.rs::helper#cfg(unix)"),
            twin("src/lib.rs::helper#cfg(not(unix))"),
        ];
        let caller = "src/lib.rs::run#cfg(all(kani,unix))";
        assert_eq!(
            chosen(&TwinView::default(), caller, &helpers),
            Some("src/lib.rs::helper#cfg(unix)")
        );
    }

    #[test]
    fn a_gate_that_does_not_parse_decides_nothing() {
        let twins = [
            twin("src/lib.rs::f#cfg(a::b)"),
            twin("src/lib.rs::f#cfg(c::d)"),
        ];
        assert_eq!(chosen(&TwinView::default(), CALLER, &twins), None);
    }
}
