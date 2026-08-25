// search::qualified_name — the shape of a stored qualified name.
//
// A stored key is `<file_path>::<rest>`, and its file path is everything
// before the FIRST `::`. Four places in this module each re-derived that one
// rule: `search::extract_file_path`, `bm25::extract_file_path`,
// `vector::extract_file_path` and `impact_target::path_part`. Every ranked
// result's `file_path` and every `get_impact` co-change lookup depends on the
// four agreeing.
//
// This is deliberately NOT `language_provider::extract_file_prefix_or_self`,
// which answers a neighbouring but different question: that one requires a
// RECOGNIZED source extension and, without one, returns the whole input
// rather than the path portion. Substituting it here would change what these
// callers see for any key whose path part carries an unrecognized extension —
// `get_impact`'s File recovery in particular walks caller-supplied input, not
// only keys the parser produced.

/// The file-path portion of a qualified name: everything before the first
/// `::`, or the whole input when it has none (a bare file path already is the
/// answer).
pub(crate) fn file_path_of(qualified_name: &str) -> &str {
    match qualified_name.find("::") {
        Some(idx) => &qualified_name[..idx],
        None => qualified_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_path() {
        assert_eq!(file_path_of("src/main.rs::handle_tool_call"), "src/main.rs");
        assert_eq!(file_path_of("src/lib.rs"), "src/lib.rs");
    }
}
