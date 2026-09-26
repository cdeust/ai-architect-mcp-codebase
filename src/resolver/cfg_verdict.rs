// resolver::cfg_verdict: whether the build compiles one `#[cfg]` twin, shared by
// the static pass (`cfg_select`) and the language-server pass (issue #366).
//
// Two facts decide it, in this order:
//
// 1. The caller's own gate. A caller whose id carries `#cfg(unix)` exists only
//    when `unix` holds, so it reaches `helper#cfg(unix)` and never
//    `helper#cfg(not(unix))`, whatever the default build says. This is
//    syntactic: the twin is compiled when every conjunct of its gate is a
//    conjunct of the caller's, and compiled out when the caller holds the
//    negation of one of its conjuncts. Nothing is solved; a gate that needs
//    reasoning stays undecided.
// 2. The default profile: the `cfg_active` column the indexer wrote from the
//    package's default features (`indexer::cfg_active`).
//
// A node that is not a twin has no gate and no `cfg_active`, so its verdict is
// `Undecided` and neither pass treats it differently.

use std::collections::HashMap;

use crate::graph_store::{cfg_gates_in, GraphStore, CFG_ACTIVE, CFG_INACTIVE};
use crate::parser::cfg_compact::parse_compact;
use crate::parser::cfg_expr::CfgPredicate;

/// `cfg_active` of every twin of the graph, read once per pass.
#[derive(Default)]
pub(crate) struct TwinView {
    active: HashMap<String, String>,
}

impl TwinView {
    pub(crate) fn load(store: &GraphStore) -> Self {
        TwinView {
            active: store.cfg_active_by_id(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with(entries: &[(&str, &str)]) -> Self {
        TwinView {
            active: entries
                .iter()
                .map(|(id, v)| (id.to_string(), v.to_string()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
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

/// The gates a caller holds, read from its qualified name or from the id of a
/// call site inside it (the site id starts with the caller's id). An
/// unparsable gate holds nothing.
pub(crate) fn caller_gates(caller: &str) -> Vec<CfgPredicate> {
    conjuncts(caller).unwrap_or_default()
}

/// Whether the build compiles the node `id` (qualified name `qn`, which carries
/// its gates) for a caller holding `caller`.
pub(crate) fn verdict(view: &TwinView, caller: &[CfgPredicate], id: &str, qn: &str) -> Verdict {
    if let Some(gate) = conjuncts(qn) {
        if !gate.is_empty() && gate.iter().all(|c| caller.contains(c)) {
            return Verdict::Compiled;
        }
        if gate.iter().any(|c| caller.contains(&negation(c))) {
            return Verdict::NotCompiled;
        }
    }
    match view.active.get(id).map(String::as_str) {
        Some(CFG_ACTIVE) => Verdict::Compiled,
        Some(CFG_INACTIVE) => Verdict::NotCompiled,
        _ => Verdict::Undecided,
    }
}

/// Whether a caller (or a call site inside it) holding the gates of `caller`
/// reaches a node the build compiles out. Used by the language-server pass,
/// which must not record a definition in a twin the build does not compile.
pub(crate) fn is_compiled_out(view: &TwinView, caller: &str, id: &str) -> bool {
    verdict(view, &caller_gates(caller), id, id) == Verdict::NotCompiled
}

/// Deletes the language-server rows to a twin `is_compiled_out` rules out for
/// their caller, before the resolve pass decides those sites again.
pub(crate) fn reset_compiled_out_lsp_rows(store: &GraphStore) -> Result<usize, String> {
    let view = TwinView::load(store);
    if view.active.is_empty() {
        return Ok(0);
    }
    store.reset_lsp_twin_rows(|caller, target| is_compiled_out(&view, caller, target))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: &str = "src/lib.rs::pick#cfg(feature=fast)";
    const SLOW: &str = "src/lib.rs::pick#cfg(not(feature=fast))";

    fn of(view: &TwinView, caller: &str, target: &str) -> Verdict {
        verdict(view, &caller_gates(caller), target, target)
    }

    #[test]
    fn the_profile_decides_a_twin_for_an_ungated_caller() {
        let view = TwinView::with(&[(FAST, "inactive"), (SLOW, "active")]);
        let site = "src/lib.rs::caller::call@3:4";
        assert_eq!(of(&view, site, FAST), Verdict::NotCompiled);
        assert_eq!(of(&view, site, SLOW), Verdict::Compiled);
        assert!(is_compiled_out(&view, site, FAST));
        assert!(!is_compiled_out(&view, site, SLOW));
    }

    #[test]
    fn a_callers_gate_outranks_the_profile() {
        let view = TwinView::with(&[(FAST, "inactive"), (SLOW, "active")]);
        let site = "src/lib.rs::run#cfg(feature=fast)::call@3:4";
        assert_eq!(of(&view, site, FAST), Verdict::Compiled);
        assert_eq!(of(&view, site, SLOW), Verdict::NotCompiled);
        assert!(!is_compiled_out(&view, site, FAST));
    }

    #[test]
    fn an_unknown_profile_and_a_plain_node_are_undecided() {
        let view = TwinView::with(&[(FAST, "unknown")]);
        let site = "src/lib.rs::caller::call@3:4";
        assert_eq!(of(&view, site, FAST), Verdict::Undecided);
        assert_eq!(of(&view, site, "src/lib.rs::plain"), Verdict::Undecided);
        assert!(!is_compiled_out(&view, site, FAST));
        assert!(!is_compiled_out(&view, site, "src/lib.rs::plain"));
    }
}
