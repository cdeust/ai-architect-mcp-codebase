// resolver::cfg_twins: recognises the candidates of a call that are twins of one
// item under mutually exclusive `#[cfg]` predicates (issue #353).
//
// The twins are distinct nodes, so the ambiguity policy sees two candidates and
// finds no evidence that separates them: two twins in the caller's file are not
// "one candidate in the caller's file", and neither is "one candidate in the
// package". The call is dropped, which is the safe answer, and this module only
// names WHY, so the site carries the reason `cfg_twins` instead of a bare
// "ambiguous (2 candidates)". A twin is never chosen here: which twin a build
// compiles is decided by the build, and only the build knows it (PR B reads the
// default features for the cases the source alone settles).

use super::SymbolEntry;
use crate::graph_store::GraphStore;
use crate::graph_store::{has_cfg_gate, strip_cfg_gates};

/// True when `candidates` are two or more twins of ONE item: the same label,
/// every one carrying a `#cfg(..)` suffix, all with the same name once the
/// suffixes are stripped. A mixed set (a twin and an unrelated symbol of the same
/// name) is an ordinary ambiguity, not this one.
pub(super) fn are_twins_of_one_item(candidates: &[SymbolEntry]) -> bool {
    let Some(first) = candidates.first() else {
        return false;
    };
    if candidates.len() < 2 {
        return false;
    }
    let shared = strip_cfg_gates(&first.qualified_name);
    candidates.iter().all(|c| {
        c.label == first.label
            && has_cfg_gate(&c.qualified_name)
            && strip_cfg_gates(&c.qualified_name) == shared
    })
}

/// Writes `unresolved_reason = cfg_twins` on the sites the resolver left open
/// because their callee has twins. The column is added first on a graph indexed
/// before it existed (same precedent as `receiver_hint`); `mark_nodes_resolved`
/// is not called for them, so they stay `is_resolved = false` and the language
/// server pass still sees them as open sites.
pub(super) fn persist_twin_reason(store: &GraphStore, ids: &[String]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    store.ensure_node_column("CallSite", "unresolved_reason", "STRING DEFAULT ''")?;
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    store.set_callsite_unresolved_reason(
        &refs,
        crate::graph_store::CALLSITE_UNRESOLVED_REASON_CFG_TWINS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, qn: &str) -> SymbolEntry {
        SymbolEntry {
            id: qn.to_string(),
            label: label.to_string(),
            qualified_name: qn.to_string(),
        }
    }

    #[test]
    fn two_gated_twins_of_one_name_are_twins() {
        let pair = [
            entry("Function", "src/lib.rs::pick#cfg(feature=fast)"),
            entry("Function", "src/lib.rs::pick#cfg(not(feature=fast))"),
        ];
        assert!(are_twins_of_one_item(&pair));
    }

    #[test]
    fn twin_methods_under_twin_types_share_one_stripped_name() {
        let pair = [
            entry("Method", "src/lib.rs::S#cfg(unix)::m"),
            entry("Method", "src/lib.rs::S#cfg(not(unix))::m"),
        ];
        assert!(are_twins_of_one_item(&pair));
    }

    #[test]
    fn a_twin_beside_an_unrelated_symbol_is_an_ordinary_ambiguity() {
        let mixed = [
            entry("Function", "src/lib.rs::pick#cfg(unix)"),
            entry("Function", "src/other.rs::pick"),
        ];
        assert!(!are_twins_of_one_item(&mixed));
    }

    #[test]
    fn different_labels_and_different_names_are_not_twins() {
        let labels = [
            entry("Function", "src/lib.rs::pick#cfg(a)"),
            entry("Method", "src/lib.rs::pick#cfg(b)"),
        ];
        assert!(!are_twins_of_one_item(&labels));
        let names = [
            entry("Function", "src/lib.rs::pick#cfg(a)"),
            entry("Function", "src/lib.rs::other#cfg(b)"),
        ];
        assert!(!are_twins_of_one_item(&names));
    }

    #[test]
    fn one_candidate_and_none_are_not_twins() {
        assert!(!are_twins_of_one_item(&[]));
        assert!(!are_twins_of_one_item(&[entry(
            "Function",
            "src/lib.rs::pick#cfg(a)"
        )]));
    }
}
