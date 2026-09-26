/// Opens the store.
pub(crate) fn open() -> &'static str {
    r#"a  "b" }"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "boom")]
    fn panics() {
        let quote = '\'';
        panic!("boom {quote}");
    }

    #[test]
    fn opens<'a>() {
        let s: &'a str = open();
        assert_eq!(s, "a  \"b\" }");
    }
}
