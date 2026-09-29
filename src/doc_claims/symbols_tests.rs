use super::*;

fn candidate(label: &'static str, visibility: &str, language: &str, trait_name: &str) -> Candidate {
    Candidate {
        id: "src/lib.rs::x".into(),
        plain: "src/lib.rs::x".into(),
        label,
        visibility: visibility.into(),
        language: language.into(),
        trait_name: trait_name.into(),
        start_line: 1,
    }
}

fn unknown_reason(c: &Candidate) -> String {
    match declared(c) {
        Declared::Unknown(why) => why,
        Declared::Pub => panic!("decided pub"),
        Declared::NotPub(why) => panic!("decided not pub: {why}"),
    }
}

#[test]
fn a_declaration_the_graph_cannot_decide_is_named_not_guessed() {
    let cases = [
        (
            candidate("Constant", "pub", "rust", ""),
            "no_visibility_recorded",
        ),
        (
            candidate("Function", "export", "typescript", ""),
            "visibility_not_checked_for_language",
        ),
        (candidate("Method", "", "rust", "Display"), "trait_method"),
        (
            candidate("Method", "", "rust", ""),
            "method_without_modifier",
        ),
        (
            candidate("Function", "pub crate", "rust", ""),
            "unrecognised_visibility",
        ),
    ];
    for (c, prefix) in cases {
        let why = unknown_reason(&c);
        assert!(
            why.starts_with(prefix),
            "{why:?} does not start with {prefix:?}"
        );
    }
}

#[test]
fn bare_pub_is_public_and_a_restriction_or_no_modifier_is_not() {
    assert!(matches!(
        declared(&candidate("Struct", "pub", "rust", "")),
        Declared::Pub
    ));
    for visibility in ["pub(crate)", "pub(super)", ""] {
        assert!(
            matches!(
                declared(&candidate("Function", visibility, "rust", "")),
                Declared::NotPub(_)
            ),
            "{visibility:?}"
        );
    }
}
