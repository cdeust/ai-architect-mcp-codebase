// parser::generic_args: the two readings of a type written with generic arguments
// (`Map<K, V>`, `Base<T>::Inner`) that the parser and the resolver share. They were
// copied into `spec::cpp_receiver`, `resolver::calls::member_calls`,
// `resolver::receiver::written_path`, `resolver::receiver` and
// `clustering::impact_other_owner` (issue #412, point 4).

/// `text` without its `<...>` groups, nesting included; every other byte stays
/// (whitespace too). An unmatched `>` is dropped and an unclosed `<` swallows the
/// rest, as the callers always did.
/// precondition: none. postcondition: the result holds no `<` and no `>`.
pub(crate) fn strip_generic_groups(text: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Strips a trailing generic-parameter list: `Wrapper<T>` -> `Wrapper`.
/// postcondition: returns `s` unchanged when it has no `<`.
pub(crate) fn strip_generics(s: &str) -> &str {
    s.split('<').next().unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_are_removed_at_any_depth_and_the_rest_is_kept() {
        assert_eq!(
            strip_generic_groups("a::Map<K, V<int>>::Inner"),
            "a::Map::Inner"
        );
        assert_eq!(strip_generic_groups(" public Base<T> "), " public Base ");
        assert_eq!(strip_generic_groups("plain"), "plain");
        assert_eq!(strip_generic_groups("a> b"), "a b");
        assert_eq!(strip_generic_groups("a<b"), "a");
    }

    #[test]
    fn a_trailing_parameter_list_is_cut() {
        assert_eq!(strip_generics("Wrapper<T>"), "Wrapper");
        assert_eq!(strip_generics("TaskSet"), "TaskSet");
        assert_eq!(strip_generics("a::B<c>::D"), "a::B");
    }
}
