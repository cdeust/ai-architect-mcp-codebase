// resolver::nested_scope — which call-target candidates a Rust call site can
// actually name when some of them are `fn` items declared inside a function
// body (issue #327).
//
// An item declared in a block is in scope throughout that block and nowhere
// else, and it shadows a same-named item or import from an outer scope. The
// parser scopes such a fn under its enclosing callable
// (`src/lib.rs::S::m::gcd`), so the rule is decidable from qualified names and
// labels alone: a candidate is NESTED when its parent QN is a `Function` or
// `Method`, and a nested candidate is visible only from a caller whose QN is
// that parent or lies under it.
// source: https://doc.rust-lang.org/reference/expressions/block-expr.html
// (items in a block), https://doc.rust-lang.org/reference/names/scopes.html
// (item scopes and shadowing).
//
// Approximation, stated: visibility is taken at FUNCTION granularity, not
// block granularity (the QN does not record the inner block a fn sits in),
// the same granularity `parser::spec::rust_scope` uses for local bindings.

use super::{SymbolEntry, SymbolIndex};

/// Labels whose body can declare a nested `fn` item.
const CALLABLE_LABELS: [&str; 2] = ["Function", "Method"];

/// The candidates a Rust call site in `caller_qn` can name.
///
/// precondition: `candidates` are the `by_name` entries for the callee's last
/// segment; `callee_is_path` is true when the callee was spelled with a path
/// (`other::gcd`, `Self::gcd`), which a nested fn can never be named by.
/// postcondition: for an unqualified callee, when the caller or one of its
/// enclosing callables declares a nested fn with that name, returns exactly
/// the nested fns of the INNERMOST such callable (block items shadow outer
/// names). Otherwise returns `candidates` minus every nested fn.
pub(super) fn visible_candidates(
    idx: &SymbolIndex,
    candidates: &[SymbolEntry],
    caller_qn: &str,
    callee_is_path: bool,
) -> Vec<SymbolEntry> {
    if !callee_is_path {
        let mut scope = Some(caller_qn);
        while let Some(owner) = scope.filter(|qn| is_callable(idx, qn)) {
            let declared: Vec<SymbolEntry> = candidates
                .iter()
                .filter(|c| c.label == "Function" && parent_qn(&c.qualified_name) == Some(owner))
                .cloned()
                .collect();
            if !declared.is_empty() {
                return declared;
            }
            scope = parent_qn(owner);
        }
    }
    candidates
        .iter()
        .filter(|c| !is_nested(idx, c))
        .cloned()
        .collect()
}

/// Whether `candidate` is a fn item declared inside a callable's body.
fn is_nested(idx: &SymbolIndex, candidate: &SymbolEntry) -> bool {
    parent_qn(&candidate.qualified_name).is_some_and(|parent| is_callable(idx, parent))
}

fn is_callable(idx: &SymbolIndex, qn: &str) -> bool {
    idx.by_qn
        .get(qn)
        .is_some_and(|e| CALLABLE_LABELS.contains(&e.label.as_str()))
}

fn parent_qn(qn: &str) -> Option<&str> {
    qn.rsplit_once("::").map(|(parent, _)| parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn entry(label: &str, qn: &str) -> SymbolEntry {
        SymbolEntry {
            id: qn.to_string(),
            label: label.to_string(),
            qualified_name: qn.to_string(),
        }
    }

    fn index(entries: &[SymbolEntry]) -> SymbolIndex {
        SymbolIndex {
            by_name: HashMap::new(),
            by_qn: entries
                .iter()
                .map(|e| (e.qualified_name.clone(), e.clone()))
                .collect(),
            by_parent_module: HashMap::new(),
        }
    }

    fn qns(entries: &[SymbolEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.qualified_name.as_str()).collect()
    }

    const M: &str = "src/lib.rs::S::m";
    const NESTED: &str = "src/lib.rs::S::m::gcd";
    const TOP: &str = "src/other.rs::gcd";

    fn fixture() -> (SymbolIndex, Vec<SymbolEntry>) {
        let nested = entry("Function", NESTED);
        let top = entry("Function", TOP);
        let idx = index(&[
            entry("Struct", "src/lib.rs::S"),
            entry("Method", M),
            entry("Function", "src/lib.rs::elsewhere"),
            nested.clone(),
            top.clone(),
        ]);
        (idx, vec![top, nested])
    }

    #[test]
    fn enclosing_method_sees_only_its_nested_fn() {
        let (idx, candidates) = fixture();
        assert_eq!(
            qns(&visible_candidates(&idx, &candidates, M, false)),
            [NESTED]
        );
    }

    #[test]
    fn nested_fn_body_sees_itself_through_its_enclosing_callable() {
        let (idx, candidates) = fixture();
        assert_eq!(
            qns(&visible_candidates(&idx, &candidates, NESTED, false)),
            [NESTED]
        );
    }

    #[test]
    fn unrelated_caller_never_sees_a_nested_fn() {
        let (idx, candidates) = fixture();
        let got = visible_candidates(&idx, &candidates, "src/lib.rs::elsewhere", false);
        assert_eq!(qns(&got), [TOP]);
    }

    #[test]
    fn path_spelled_callee_never_names_a_nested_fn() {
        let (idx, candidates) = fixture();
        assert_eq!(qns(&visible_candidates(&idx, &candidates, M, true)), [TOP]);
    }
}
