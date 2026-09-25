// clustering::impact_context: which callers of a target are production code
// (issue #354).
//
// `get_impact` lists every caller in one list and counts them in
// `callers_total`. Test, bench and example functions are callers like any other,
// so on a well-tested function most of the count is not production. This module
// gives each caller a `context` (`production`, `test`, `bench`, `example`,
// `proof` or `unknown`, see `graph_store::code_context`) and summarizes them.
//
// `unknown` counts as production in `production_total`: the count shrinks only
// when a caller is PROVEN non-production, never on missing evidence. The list and
// `callers_total` are not changed, so nobody loses a caller.

use std::collections::BTreeMap;

use super::impact::ImpactNode;
use crate::graph_store::code_context::{CONTEXT_PRODUCTION, CONTEXT_UNKNOWN};
use crate::graph_store::GraphStore;

/// Sets `context` on every caller and returns where the answer comes from
/// (`cargo+source`, `source_only`, `absent`).
pub(super) fn attach(store: &GraphStore, callers: &mut [ImpactNode]) -> &'static str {
    let contexts = {
        let handles: Vec<(&str, &str)> = callers
            .iter()
            .map(|c| (c.label.as_str(), c.id.as_str()))
            .collect();
        store.caller_contexts(&handles)
    };
    for caller in callers.iter_mut() {
        caller.context = contexts
            .get(&caller.id)
            .cloned()
            .unwrap_or_else(|| CONTEXT_UNKNOWN.to_string());
    }
    store.code_context_basis()
}

/// The callers of a target split by context.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CallerContexts {
    /// How many callers each context holds.
    pub by_context: BTreeMap<String, u64>,
    /// Callers not proven non-production: `production` plus `unknown`.
    pub production_total: usize,
}

/// Counts `callers` by context. A caller with no context (a graph read before
/// `attach`) is `unknown`.
pub fn summarize(callers: &[ImpactNode]) -> CallerContexts {
    let mut summary = CallerContexts::default();
    for caller in callers {
        let context = if caller.context.is_empty() {
            CONTEXT_UNKNOWN
        } else {
            caller.context.as_str()
        };
        *summary.by_context.entry(context.to_string()).or_default() += 1;
        if context == CONTEXT_PRODUCTION || context == CONTEXT_UNKNOWN {
            summary.production_total += 1;
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(context: &str) -> ImpactNode {
        ImpactNode {
            id: format!("src/lib.rs::{context}"),
            qualified_name: format!("src/lib.rs::{context}"),
            label: "Function".to_string(),
            confidence: 1.0,
            context: context.to_string(),
        }
    }

    #[test]
    fn unknown_and_production_are_the_production_total_and_the_rest_is_proven_away() {
        let callers: Vec<ImpactNode> = ["production", "unknown", "test", "test", "bench", ""]
            .into_iter()
            .map(caller)
            .collect();
        let summary = summarize(&callers);
        assert_eq!(summary.production_total, 3);
        assert_eq!(summary.by_context["test"], 2);
        assert_eq!(summary.by_context["unknown"], 2);
        assert_eq!(summary.by_context["production"], 1);
        assert_eq!(summary.by_context["bench"], 1);
    }

    #[test]
    fn a_target_without_callers_has_no_context() {
        assert_eq!(summarize(&[]), CallerContexts::default());
    }
}
