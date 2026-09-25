// resolver::cfg_select: picks, among the twins of one item, the one a build
// compiles, when the build decides it (issue #353, part B).
//
// `cfg_twins` leaves a call to a twin set open, because the twins are distinct
// nodes and nothing separates them. This module reads the two facts that can:
//
// 1. The caller's own gate. A caller whose id carries `#cfg(unix)` exists only
//    when `unix` holds, so among `helper#cfg(unix)` and `helper#cfg(not(unix))`
//    it reaches the first, whatever the default build says. This is syntactic:
//    the twin is compiled when every conjunct of its gate is a conjunct of the
//    caller's, and is compiled out when the caller holds the negation of one of
//    its conjuncts. Nothing is solved; a gate that needs reasoning stays open.
// 2. The default profile: the `cfg_active` column the indexer wrote from the
//    package's default features (`indexer::cfg_active`).
//
// A twin is chosen only when exactly one twin is not ruled out AND that twin is
// shown compiled. A lone survivor that is merely undecided is not chosen: two
// twins are not guaranteed to be complementary, and a call must not be pinned
// to a twin the build might not compile. Every case that is not decided leaves
// the site open with the reason `cfg_twins`, as before.

use std::collections::HashMap;

use super::SymbolEntry;
use crate::graph_store::{cfg_gates_in, GraphStore, CFG_ACTIVE, CFG_INACTIVE};
use crate::parser::cfg_compact::parse_compact;
use crate::parser::cfg_expr::CfgPredicate;

/// `cfg_active` of every twin of the graph, read once per resolve pass.
#[derive(Default)]
pub(super) struct TwinView {
    active: HashMap<String, String>,
}

impl TwinView {
    pub(super) fn load(store: &GraphStore) -> Self {
        TwinView {
            active: store.cfg_active_by_id(),
        }
    }

    #[cfg(test)]
    pub(super) fn with(entries: &[(&str, &str)]) -> Self {
        TwinView {
            active: entries
                .iter()
                .map(|(id, v)| (id.to_string(), v.to_string()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Compiled,
    NotCompiled,
    Undecided,
}

/// The conjuncts of the gates written in `qn`, or `None` when one gate does not
/// parse (nothing is then known about the item).
fn conjuncts(qn: &str) -> Option<Vec<CfgPredicate>> {
    let mut out: Vec<CfgPredicate> = Vec::new();
    for gate in cfg_gates_in(qn) {
        let parts = match parse_compact(gate)? {
            CfgPredicate::All(items) => items,
            single => vec![single],
        };
        for part in parts {
            if !out.contains(&part) {
                out.push(part);
            }
        }
    }
    Some(out)
}

fn negation(predicate: &CfgPredicate) -> CfgPredicate {
    match predicate {
        CfgPredicate::Not(inner) => (**inner).clone(),
        other => CfgPredicate::Not(Box::new(other.clone())),
    }
}

fn verdict(view: &TwinView, caller: &[CfgPredicate], twin: &SymbolEntry) -> Verdict {
    if let Some(gate) = conjuncts(&twin.qualified_name) {
        if !gate.is_empty() && gate.iter().all(|c| caller.contains(c)) {
            return Verdict::Compiled;
        }
        if gate.iter().any(|c| caller.contains(&negation(c))) {
            return Verdict::NotCompiled;
        }
    }
    match view.active.get(&twin.id).map(String::as_str) {
        Some(CFG_ACTIVE) => Verdict::Compiled,
        Some(CFG_INACTIVE) => Verdict::NotCompiled,
        _ => Verdict::Undecided,
    }
}

/// The twin a call to `candidates` reaches, when every candidate is a twin of
/// one item and the build decides which one it compiles; `None` otherwise (an
/// ordinary ambiguity, or a twin set the build leaves open).
pub(super) fn choose<'a>(
    view: &TwinView,
    caller_qn: &str,
    candidates: &'a [SymbolEntry],
) -> Option<&'a SymbolEntry> {
    super::cfg_twins::are_twins_of_one_item(candidates)
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
    let caller = conjuncts(caller_qn).unwrap_or_default();
    let mut not_ruled_out = twins
        .iter()
        .map(|twin| (twin, verdict(view, &caller, twin)))
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
