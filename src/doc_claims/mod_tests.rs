use super::*;

#[test]
fn verdict_names_are_the_wire_names() {
    let names: Vec<&str> = Verdict::ALL.iter().map(|v| v.as_str()).collect();
    assert_eq!(
        names,
        [
            "supported",
            "contradicted",
            "not_found",
            "not_verifiable",
            "rejected_anchor"
        ]
    );
}

#[test]
fn a_row_serializes_every_field_including_an_absent_reason() {
    let row = Row {
        claim: Claim {
            id: "c0".into(),
            text: "8 tests".into(),
            file: "README.md".into(),
            line: 3,
            kind: "test_count".into(),
            subject: String::new(),
            expected: "8".into(),
        },
        outcome: Outcome::new(
            Verdict::Supported,
            None,
            vec![graph_evidence("Q", json!(8))],
        ),
    };
    let v = row.to_json();
    assert_eq!(v["verdict"], "supported");
    assert!(v["reason"].is_null());
    assert_eq!(v["evidence"][0]["query"], "Q");
    assert_eq!(v["line"], 3);
}

#[test]
fn every_supported_kind_is_dispatched() {
    // A kind listed as supported must never fall through to
    // `kind_not_supported_yet`; the dispatch table and the list are one fact.
    let source = include_str!("mod.rs");
    for kind in SUPPORTED_KINDS {
        assert!(
            source.contains(&format!("\"{kind}\" =>")),
            "{kind} has no dispatch arm"
        );
    }
}
