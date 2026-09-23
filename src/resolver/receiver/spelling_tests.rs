// resolver::receiver::spelling_tests — per-language receiver spelling
// (`self.`/`this.`/`Self::`) for `classify`; split from mod.rs for the §4.1
// 500-line cap. source: issue #290.

use super::*;

fn spelling(language: &str) -> ReceiverSpelling {
    ReceiverSpelling::of(crate::language_provider::provider_for(language))
}

#[test]
fn python_self_and_typescript_this_classify_as_same_class_receivers() {
    let m = ReceiverForm::SelfValue("fetch".to_string());
    assert_eq!(classify("self.fetch", &spelling("python")), m);
    assert_eq!(classify("this.fetch", &spelling("typescript")), m);
}

#[test]
fn a_language_never_borrows_another_languages_receiver_spelling() {
    // `this.` is not a receiver in Python or Rust, `self.` is not one in
    // TypeScript, and `Self::` exists only in Rust: each falls to the
    // spelling-only `Local` arm or to `None`, never to `SelfValue`.
    assert!(matches!(
        classify("this.fetch", &spelling("python")),
        ReceiverForm::Local { .. }
    ));
    assert!(matches!(
        classify("self.fetch", &spelling("typescript")),
        ReceiverForm::Local { .. }
    ));
    assert_eq!(
        classify("Self::new", &spelling("python")),
        ReceiverForm::None
    );
}

#[test]
fn chained_this_member_is_none_not_local() {
    // `this.props.onClick` — the receiver is a FIELD, not the class.
    assert_eq!(
        classify("this.props.onClick", &spelling("typescript")),
        ReceiverForm::None
    );
}

#[test]
fn languages_that_did_not_opt_in_have_no_same_class_spelling() {
    for lang in ["java", "kotlin", "go", "swift", "cpp"] {
        assert!(
            !spelling(lang).binds_same_class_receiver(),
            "{lang} must not bind receivers until it opts in"
        );
    }
    for lang in ["rust", "python", "typescript"] {
        assert!(spelling(lang).binds_same_class_receiver(), "{lang}");
    }
}
