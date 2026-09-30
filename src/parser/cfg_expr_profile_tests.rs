// Issue #391: the default build profile fixes the options no plain `cargo build`
// ever sets (`kani`, `miri`, `doc`, `doctest`) to false; a Kani profile sets
// `kani` true; every other option stays `Unknown`, and `all`/`any`/`not` compose
// over the three values.

use super::*;

fn profile(features: &[&str]) -> BuildProfile {
    BuildProfile::with_features(features.iter().map(|f| f.to_string()).collect())
}

fn kani_profile() -> BuildProfile {
    let mut p = profile(&[]);
    p.options.insert("kani".to_string(), true);
    p
}

fn under(text: &str, p: &BuildProfile) -> Truth {
    parse_cfg_arguments(text).expect("parses").eval_in(p)
}

fn default_of(text: &str) -> Truth {
    under(text, &profile(&[]))
}

#[test]
fn the_default_profile_fixes_the_options_no_plain_build_sets_to_false() {
    for option in ["kani", "miri", "doc", "doctest"] {
        assert_eq!(default_of(&format!("({option})")), Truth::False, "{option}");
        assert_eq!(
            default_of(&format!("(not({option}))")),
            Truth::True,
            "not({option})"
        );
    }
}

#[test]
fn the_default_profile_leaves_every_other_option_unknown() {
    for text in [
        "(unix)",
        "(windows)",
        "(test)",
        "(debug_assertions)",
        "(target_os = \"linux\")",
        "(target_pointer_width = \"64\")",
        "(not(unix))",
        "(not(test))",
    ] {
        assert_eq!(default_of(text), Truth::Unknown, "{text}");
    }
}

#[test]
fn a_valued_option_of_a_fixed_name_is_not_the_bare_option() {
    assert_eq!(default_of("(kani = \"yes\")"), Truth::Unknown);
}

#[test]
fn all_over_a_false_option_is_false_even_beside_an_unknown() {
    assert_eq!(default_of("(all(kani, unix))"), Truth::False);
    assert_eq!(default_of("(all(unix, kani))"), Truth::False);
    assert_eq!(default_of("(not(all(kani, unix)))"), Truth::True);
}

#[test]
fn a_conjunction_with_an_unknown_stays_unknown() {
    assert_eq!(default_of("(all(not(kani), unix))"), Truth::Unknown);
    assert_eq!(default_of("(all(not(kani), not(miri)))"), Truth::True);
    assert_eq!(default_of("(not(all(not(kani), unix)))"), Truth::Unknown);
}

#[test]
fn any_over_a_false_option_is_decided_only_by_its_other_branches() {
    assert_eq!(default_of("(any(kani, unix))"), Truth::Unknown);
    assert_eq!(default_of("(any(kani, miri))"), Truth::False);
    assert_eq!(default_of("(any(not(kani), unix))"), Truth::True);
    assert_eq!(default_of("(not(any(kani, miri)))"), Truth::True);
}

#[test]
fn features_compose_with_a_fixed_option() {
    let on = profile(&["x"]);
    assert_eq!(under("(all(feature = \"x\", not(kani)))", &on), Truth::True);
    assert_eq!(under("(all(feature = \"x\", kani))", &on), Truth::False);
    assert_eq!(
        default_of("(all(feature = \"x\", not(kani)))"),
        Truth::False
    );
    assert_eq!(under("(any(feature = \"x\", kani))", &on), Truth::True);
    assert_eq!(default_of("(any(feature = \"x\", kani))"), Truth::False);
}

#[test]
fn a_kani_profile_inverts_kani_and_only_kani() {
    let p = kani_profile();
    assert_eq!(under("(kani)", &p), Truth::True);
    assert_eq!(under("(not(kani))", &p), Truth::False);
    assert_eq!(under("(miri)", &p), Truth::False);
    assert_eq!(under("(unix)", &p), Truth::Unknown);
    assert_eq!(under("(all(kani, unix))", &p), Truth::Unknown);
    assert_eq!(under("(any(kani, unix))", &p), Truth::True);
    assert_eq!(under("(all(not(kani), unix))", &p), Truth::False);
}

#[test]
fn a_feature_set_alone_never_decides_an_option_of_the_bare_evaluation() {
    // `eval` reads features only: the module-reachability passes keep every
    // option `Unknown`, so a `#[cfg(kani)] mod proofs;` is never compiled out.
    let enabled = BTreeSet::new();
    let p = parse_cfg_arguments("(kani)").expect("parses");
    assert_eq!(p.eval(&enabled), Truth::Unknown);
}
