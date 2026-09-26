/// Opens the store.
pub(crate) fn open() -> &'static str {
    r#"a  "b" }"#
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
