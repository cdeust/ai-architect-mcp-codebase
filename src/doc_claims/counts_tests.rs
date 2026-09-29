use super::*;

#[test]
fn expected_counts_parse_exact_and_at_least_only() {
    assert_eq!(parse_expected(" 26 "), Some(Expected::Exact(26)));
    assert_eq!(parse_expected(">=8"), Some(Expected::AtLeast(8)));
    assert_eq!(parse_expected(">= 8"), Some(Expected::AtLeast(8)));
    assert_eq!(parse_expected("eight"), None);
    assert_eq!(parse_expected("-1"), None);
    assert_eq!(parse_expected(""), None);
}

#[test]
fn a_scope_is_everything_a_directory_or_one_file() {
    assert_eq!(scope_prefix(""), None);
    assert_eq!(scope_prefix("."), None);
    assert_eq!(scope_prefix("tests/"), Some("tests/".into()));
    assert_eq!(scope_prefix("./src/lib.rs"), Some("src/lib.rs::".into()));
}

fn gate(line: u64, gate: &str) -> FunctionGate {
    FunctionGate {
        line,
        gate: gate.into(),
        cfg_attr: false,
        nested: false,
    }
}

#[test]
fn only_the_harness_option_may_gate_a_floor_function() {
    let gates = vec![
        gate(1, ""),
        gate(2, "test"),
        gate(3, "unix"),
        gate(4, "kani"),
    ];
    assert_eq!(function_verdict(&gates, 1, &TESTS), None);
    assert_eq!(function_verdict(&gates, 2, &TESTS), None);
    assert_eq!(
        function_verdict(&gates, 3, &TESTS),
        Some("gated(unix)".into())
    );
    assert_eq!(
        function_verdict(&gates, 4, &TESTS),
        Some("gated(kani)".into())
    );
    assert_eq!(function_verdict(&gates, 4, &PROOFS), None);
    assert_eq!(
        function_verdict(&gates, 9, &TESTS),
        Some("not_at_graph_line".into())
    );
}

#[test]
fn nested_and_cfg_attr_functions_leave_the_floor() {
    let mut nested = gate(1, "");
    nested.nested = true;
    let mut attr = gate(2, "");
    attr.cfg_attr = true;
    let gates = vec![nested, attr];
    assert_eq!(
        function_verdict(&gates, 1, &TESTS),
        Some("nested_function".into())
    );
    assert_eq!(function_verdict(&gates, 2, &TESTS), Some("cfg_attr".into()));
}

#[test]
fn a_bad_expected_value_is_not_verifiable_with_the_syntax() {
    let o = bad_expected("many");
    assert_eq!(o.verdict, Verdict::NotVerifiable);
    assert!(o.reason.expect("reason").contains(">=N"));
}

fn shape(declared: u64, gated: u64) -> VariantShape {
    VariantShape { declared, gated }
}

#[test]
fn a_variant_count_is_contradicted_only_outside_ungated_to_declared() {
    let v = |expected, s| variant_verdict(expected, s, "n", Vec::new()).verdict;
    assert_eq!(v(Expected::Exact(3), shape(3, 0)), Verdict::Supported);
    assert_eq!(v(Expected::Exact(2), shape(3, 0)), Verdict::Contradicted);
    assert_eq!(v(Expected::Exact(4), shape(3, 0)), Verdict::Contradicted);
    // One of three under its own cfg: every build has 2 or 3.
    assert_eq!(v(Expected::Exact(2), shape(3, 1)), Verdict::NotVerifiable);
    assert_eq!(v(Expected::Exact(3), shape(3, 1)), Verdict::NotVerifiable);
    assert_eq!(v(Expected::Exact(1), shape(3, 1)), Verdict::Contradicted);
    assert_eq!(v(Expected::AtLeast(2), shape(3, 1)), Verdict::Supported);
    assert_eq!(v(Expected::AtLeast(3), shape(3, 1)), Verdict::NotVerifiable);
    assert_eq!(v(Expected::AtLeast(4), shape(3, 1)), Verdict::Contradicted);
}

#[test]
fn an_enum_of_an_unread_language_can_be_supported_never_contradicted() {
    let v = |expected, declared| unread_language_verdict(expected, declared, "ts", Vec::new());
    assert_eq!(v(Expected::Exact(2), 2).verdict, Verdict::Supported);
    assert_eq!(v(Expected::AtLeast(1), 2).verdict, Verdict::Supported);
    for expected in [Expected::Exact(3), Expected::Exact(1), Expected::AtLeast(3)] {
        let o = v(expected, 2);
        assert_eq!(o.verdict, Verdict::NotVerifiable);
        assert!(o
            .reason
            .expect("reason")
            .starts_with("variant_completeness_not_established"));
    }
}

#[test]
fn a_harness_count_is_contradicted_only_when_the_floor_exceeds_it() {
    let v = |expected, declared, floor| count_verdict(expected, declared, floor, Vec::new());
    let reason = |o: Outcome| o.reason.unwrap_or_default();
    assert_eq!(v(Expected::Exact(4), 4, 4).verdict, Verdict::Supported);
    assert!(reason(v(Expected::Exact(3), 5, 4)).starts_with("floor_exceeds_claim"));
    assert!(reason(v(Expected::Exact(9), 5, 4)).starts_with("lower_bound"));
    assert!(reason(v(Expected::Exact(5), 5, 4)).starts_with("build_dependent"));
    assert_eq!(v(Expected::AtLeast(4), 5, 4).verdict, Verdict::Supported);
    assert!(reason(v(Expected::AtLeast(9), 5, 4)).starts_with("lower_bound"));
    assert!(reason(v(Expected::AtLeast(5), 5, 4)).starts_with("build_dependent"));
}

#[test]
fn the_source_must_still_hold_the_indexed_enum() {
    let tmp = tempfile::tempdir().expect("tmp");
    std::fs::create_dir_all(tmp.path().join("src")).expect("src");
    std::fs::write(
        tmp.path().join("src/lib.rs"),
        "pub enum E {\n    A,\n    #[cfg(unix)]\n    B,\n}\n",
    )
    .expect("lib");
    let mut files = RepoFiles::new(tmp.path()).expect("root");
    let gated = rust_gated_variants(&mut files, "src/lib.rs::E", 1, 2).expect("shape");
    assert_eq!(gated, (1, "src/lib.rs:1".to_string()));
    let err = |id, line, declared| {
        rust_gated_variants(
            &mut RepoFiles::new(tmp.path()).expect("root"),
            id,
            line,
            declared,
        )
        .unwrap_err()
    };
    assert!(err("src/gone.rs::E", 1, 2).starts_with("source_unreadable"));
    assert!(err("src/lib.rs::E", 3, 2).starts_with("source_changed_since_indexing"));
    assert!(err("src/lib.rs::E", 1, 3).starts_with("graph_and_source_disagree"));
}
