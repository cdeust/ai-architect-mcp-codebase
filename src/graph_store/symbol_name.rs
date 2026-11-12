// graph_store::symbol_name: the bare name a symbol id or a callee carries.

/// The last segment of a symbol id or a callee name (`model.go::Count`,
/// `pkg.Count`, `a::b::Count`), without a `#n` disambiguator: what a call site
/// that names a type and the type's node have in common (Codex review of #523).
pub(crate) fn symbol_name(id: &str) -> &str {
    let tail = id.rsplit("::").next().unwrap_or(id);
    let tail = tail.rsplit('.').next().unwrap_or(tail);
    tail.split('#').next().unwrap_or(tail)
}

#[cfg(test)]
mod tests {
    use super::symbol_name;

    #[test]
    fn an_id_and_a_callee_that_name_one_type_give_one_name() {
        assert_eq!(symbol_name("model.go::Count"), "Count");
        assert_eq!(symbol_name("pkg.Count"), "Count");
        assert_eq!(symbol_name("a.rs::m::Count#2"), "Count");
        assert_eq!(symbol_name("Count"), "Count");
    }
}
