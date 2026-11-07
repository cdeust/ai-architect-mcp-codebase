// Tests for wiki::skeleton — issue #196, criteria 3 to 7, plus the criteria
// 1–2 tripwire (artifact slice).

use super::*;

fn loc(file: &str, line: u32) -> Location {
    Location::new(file, line).expect("test locations are valid")
}

fn symbol(id: &str, file: &str, line: u32) -> Symbol {
    Symbol {
        id: id.to_owned(),
        name: id.to_owned(),
        kind: "Function".to_owned(),
        location: loc(file, line),
    }
}

fn community_page(gaps: Vec<CoverageGap>) -> Page {
    let mut page = Page::new(PageKind::Community, "c1", "parser");
    page.members = vec![symbol("b", "src/b.rs", 7), symbol("a", "src/a.rs", 3)];
    page.edges = vec![
        Edge::new(
            ("a", "b"),
            "Calls",
            Provenance::Inferred,
            loc("src/a.rs", 4),
        ),
        Edge::new(
            ("a", "b"),
            "Defines",
            Provenance::Extracted,
            loc("src/a.rs", 3),
        ),
    ];
    page.cross_community_edges.insert("c2".to_owned(), 5);
    page.coverage = gaps;
    page
}

fn gaps() -> Vec<CoverageGap> {
    vec![
        CoverageGap::ParseIncomplete {
            file: "src/broken.rs".to_owned(),
        },
        CoverageGap::Missed {
            name: "unknown_fn".to_owned(),
            location: loc("src/a.rs", 9),
        },
    ]
}

#[test]
fn a_location_refuses_line_zero_and_an_empty_file() {
    assert_eq!(Location::new("src/a.rs", 0), Err(SkeletonError::ZeroLine));
    assert_eq!(Location::new("", 1), Err(SkeletonError::EmptyFile));
    let ok = loc("src/a.rs", 1);
    assert_eq!((ok.file(), ok.line()), ("src/a.rs", 1));
}

#[test]
fn every_edge_is_serialised_with_one_of_the_two_provenance_tags() {
    let json = WikiSkeleton::new(vec![community_page(vec![])])
        .unwrap()
        .to_json()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let edges = value["pages"][0]["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 2);
    let tags: Vec<&str> = edges
        .iter()
        .map(|e| e["provenance"].as_str().unwrap())
        .collect();
    assert_eq!(tags, ["inferred", "extracted"]);
}

#[test]
fn an_edge_without_or_with_an_unknown_provenance_does_not_deserialise() {
    let edge = r#"{"source":"a","target":"b","relation":"Calls","location":{"file":"f","line":1}"#;
    assert!(serde_json::from_str::<Edge>(&format!("{edge}}}")).is_err());
    assert!(serde_json::from_str::<Edge>(&format!(r#"{edge},"provenance":"guessed"}}"#)).is_err());
    let tagged = format!(r#"{edge},"provenance":"inferred"}}"#);
    let parsed: Edge = serde_json::from_str(&tagged).unwrap();
    assert_eq!(parsed.provenance(), Provenance::Inferred);
}

#[test]
fn a_location_with_line_zero_or_an_empty_file_does_not_deserialise() {
    assert!(serde_json::from_str::<Location>(r#"{"file":"f","line":0}"#).is_err());
    assert!(serde_json::from_str::<Location>(r#"{"file":"","line":1}"#).is_err());
    assert!(serde_json::from_str::<Location>(r#"{"file":"f"}"#).is_err());
}

#[test]
fn every_structural_claim_carries_file_and_line() {
    let json = WikiSkeleton::new(vec![community_page(gaps())])
        .unwrap()
        .to_json()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let page = &value["pages"][0];
    let anchored = |v: &serde_json::Value| v["file"].is_string() && v["line"].as_u64() >= Some(1);
    for member in page["members"].as_array().unwrap() {
        assert!(
            anchored(&member["location"]),
            "member without file:line: {member}"
        );
    }
    for edge in page["edges"].as_array().unwrap() {
        assert!(
            anchored(&edge["location"]),
            "edge without file:line: {edge}"
        );
    }
    let missed: Vec<_> = page["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["kind"] == "missed")
        .collect();
    assert!(
        !missed.is_empty(),
        "the fixture page has a missed-graph gap"
    );
    for gap in missed {
        assert!(anchored(&gap["location"]), "gap without file:line: {gap}");
    }
}

#[test]
fn an_empty_coverage_block_is_present_not_absent() {
    let json = WikiSkeleton::new(vec![community_page(vec![])])
        .unwrap()
        .to_json()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let coverage = value["pages"][0]
        .get("coverage")
        .expect("coverage key is always written");
    assert_eq!(coverage, &serde_json::json!([]));
}

#[test]
fn a_populated_coverage_block_lists_every_gap_in_canonical_order() {
    let json = WikiSkeleton::new(vec![community_page(gaps())])
        .unwrap()
        .to_json()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let expected = serde_json::json!([
        {"kind": "missed", "name": "unknown_fn", "location": {"file": "src/a.rs", "line": 9}},
        {"kind": "parse_incomplete", "file": "src/broken.rs"},
    ]);
    assert_eq!(value["pages"][0]["coverage"], expected);
}

#[test]
fn the_same_graph_in_any_order_gives_byte_identical_json() {
    let mut reversed = community_page(gaps());
    reversed.members.reverse();
    reversed.edges.reverse();
    reversed.coverage.reverse();
    reversed.members.push(reversed.members[0].clone());
    reversed.edges.push(reversed.edges[0].clone());
    reversed.coverage.push(reversed.coverage[0].clone());
    let process = Page::new(PageKind::Process, "p1", "main flow");
    let god = Page::new(PageKind::GodNode, "g1", "GraphStore");
    let first = WikiSkeleton::new(vec![community_page(gaps()), process.clone(), god.clone()]);
    let second = WikiSkeleton::new(vec![god, reversed, process]);
    assert_eq!(
        first.unwrap().to_json().unwrap(),
        second.unwrap().to_json().unwrap()
    );
}

#[test]
fn cross_community_counts_serialise_in_key_order_whatever_the_insertion_order() {
    let keys = ["c9", "c2", "c5", "c3"];
    let mut forward = Page::new(PageKind::Community, "c1", "x");
    let mut backward = Page::new(PageKind::Community, "c1", "x");
    for (n, key) in keys.iter().enumerate() {
        forward
            .cross_community_edges
            .insert((*key).to_owned(), n as u64);
    }
    for (n, key) in keys.iter().enumerate().rev() {
        backward
            .cross_community_edges
            .insert((*key).to_owned(), n as u64);
    }
    let a = WikiSkeleton::new(vec![forward]).unwrap().to_json().unwrap();
    let b = WikiSkeleton::new(vec![backward])
        .unwrap()
        .to_json()
        .unwrap();
    assert_eq!(a, b);
    let positions: Vec<usize> = ["c2", "c3", "c5", "c9"]
        .iter()
        .map(|k| a.find(&format!("\"{k}\"")).unwrap())
        .collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{a}");
}

/// Pins the v1 bytes: a field rename, a field reorder or a formatting change
/// must come with a `SCHEMA_VERSION` bump and a new fixture.
#[test]
fn the_v1_output_matches_the_golden_fixture_byte_for_byte() {
    let golden = include_str!("testdata/skeleton_v1.json");
    let built = WikiSkeleton::new(vec![
        community_page(gaps()),
        Page::new(PageKind::GodNode, "g1", "GraphStore"),
    ]);
    assert_eq!(built.unwrap().to_json().unwrap(), golden);
    assert_eq!(
        WikiSkeleton::from_json(golden).unwrap().to_json().unwrap(),
        golden
    );
}

#[test]
fn source_files_are_derived_from_the_members() {
    let skeleton = WikiSkeleton::new(vec![community_page(vec![])]).unwrap();
    assert_eq!(skeleton.pages()[0].source_files(), ["src/a.rs", "src/b.rs"]);
    // `new` overwrites a stored value...
    let mut stale = community_page(vec![]);
    stale.source_files = vec!["stale.rs".to_owned()];
    let fixed = WikiSkeleton::new(vec![stale]).unwrap();
    assert_eq!(fixed.pages()[0].source_files(), ["src/a.rs", "src/b.rs"]);
    // ...so pages taken from a skeleton can be edited and rebuilt.
    let mut pages = fixed.pages().to_vec();
    pages[0].members.push(symbol("z", "src/new.rs", 1));
    pages[0].members.retain(|m| m.id != "b");
    let rebuilt = WikiSkeleton::new(pages).unwrap();
    assert_eq!(
        rebuilt.pages()[0].source_files(),
        ["src/a.rs", "src/new.rs"]
    );
}

/// claude[bot] review on #475: on read, `source_files` is required and a
/// value that disagrees (an explicit `[]` included) is refused.
#[test]
fn a_read_source_files_that_disagrees_or_is_missing_is_refused() {
    let golden = include_str!("testdata/skeleton_v1.json");
    let stale = golden.replacen("\"src/b.rs\"\n      ]", "\"src/c.rs\"\n      ]", 1);
    let emptied = golden.replacen(
        "\"source_files\": [\n        \"src/a.rs\",\n        \"src/b.rs\"\n      ]",
        "\"source_files\": []",
        1,
    );
    let missing = golden.replacen(
        "\"source_files\": [\n        \"src/a.rs\",\n        \"src/b.rs\"\n      ],\n",
        "",
        1,
    );
    for (case, text) in [
        ("stale", &stale),
        ("emptied", &emptied),
        ("missing", &missing),
    ] {
        assert_ne!(
            text.as_str(),
            golden,
            "{case}: the fixture must contain the replaced text"
        );
    }
    let refused = SkeletonError::StaleSourceFiles { page: "c1".into() };
    assert_eq!(WikiSkeleton::from_json(&stale), Err(refused.clone()));
    assert_eq!(WikiSkeleton::from_json(&emptied), Err(refused));
    let err = WikiSkeleton::from_json(&missing).unwrap_err();
    assert!(
        matches!(&err, SkeletonError::Json(m) if m.contains("source_files")),
        "{err:?}"
    );
}

#[test]
fn pages_are_ordered_by_kind_then_id_and_duplicates_are_refused() {
    let pages = vec![
        Page::new(PageKind::GodNode, "g1", "x"),
        Page::new(PageKind::Community, "c2", "x"),
        Page::new(PageKind::Community, "c1", "x"),
    ];
    let skeleton = WikiSkeleton::new(pages).unwrap();
    let order: Vec<&str> = skeleton.pages().iter().map(|p| p.id.as_str()).collect();
    assert_eq!(order, ["c1", "c2", "g1"]);
    // All three kinds, and one id under two kinds: both pages coexist.
    let mixed = vec![
        Page::new(PageKind::GodNode, "x", "x"),
        Page::new(PageKind::Process, "x", "x"),
        Page::new(PageKind::Community, "x", "x"),
    ];
    let skeleton = WikiSkeleton::new(mixed).unwrap();
    let kinds: Vec<PageKind> = skeleton.pages().iter().map(|p| p.kind).collect();
    assert_eq!(
        kinds,
        [PageKind::Community, PageKind::Process, PageKind::GodNode]
    );
    let twice = vec![
        Page::new(PageKind::Process, "p", "a"),
        Page::new(PageKind::Process, "p", "b"),
    ];
    let err = WikiSkeleton::new(twice).unwrap_err();
    assert_eq!(
        err,
        SkeletonError::DuplicatePage {
            kind: PageKind::Process,
            id: "p".into()
        }
    );
}

#[test]
fn empty_ids_and_empty_gap_files_are_refused() {
    let no_id = Page::new(PageKind::Community, "", "x");
    assert_eq!(
        WikiSkeleton::new(vec![no_id]),
        Err(SkeletonError::EmptyId("page"))
    );
    let mut no_member_id = Page::new(PageKind::Community, "c", "x");
    no_member_id.members = vec![symbol("", "src/a.rs", 1)];
    assert_eq!(
        WikiSkeleton::new(vec![no_member_id]),
        Err(SkeletonError::EmptyId("member symbol"))
    );
    let mut no_gap_file = Page::new(PageKind::Community, "c", "x");
    no_gap_file.coverage = vec![CoverageGap::ParseIncomplete {
        file: String::new(),
    }];
    assert_eq!(
        WikiSkeleton::new(vec![no_gap_file]),
        Err(SkeletonError::EmptyFile)
    );
}

#[test]
fn empty_edge_fields_cross_keys_and_missed_names_are_refused() {
    let edge = |ends: (&str, &str), relation: &str| {
        let mut page = Page::new(PageKind::Community, "c", "x");
        page.edges = vec![Edge::new(ends, relation, Provenance::Inferred, loc("f", 1))];
        WikiSkeleton::new(vec![page])
    };
    assert_eq!(
        edge(("", "b"), "Calls"),
        Err(SkeletonError::EmptyId("edge source"))
    );
    assert_eq!(
        edge(("a", ""), "Calls"),
        Err(SkeletonError::EmptyId("edge target"))
    );
    assert_eq!(
        edge(("a", "b"), ""),
        Err(SkeletonError::EmptyId("edge relation"))
    );
    let mut key = Page::new(PageKind::Community, "c", "x");
    key.cross_community_edges.insert(String::new(), 1);
    assert_eq!(
        WikiSkeleton::new(vec![key]),
        Err(SkeletonError::EmptyId("cross-community key"))
    );
    let mut own = Page::new(PageKind::Community, "c", "x");
    own.cross_community_edges.insert("c".to_owned(), 1);
    let err = WikiSkeleton::new(vec![own]).unwrap_err();
    assert_eq!(err, SkeletonError::SelfCrossCommunity { page: "c".into() });
    let mut missed = Page::new(PageKind::Community, "c", "x");
    missed.coverage = vec![CoverageGap::Missed {
        name: String::new(),
        location: loc("f", 2),
    }];
    let err = WikiSkeleton::new(vec![missed]).unwrap_err();
    assert_eq!(err, SkeletonError::EmptyId("missed-graph entry"));
}

#[test]
fn two_members_sharing_an_id_are_refused_even_when_other_fields_differ() {
    let mut page = Page::new(PageKind::Community, "c", "x");
    page.members = vec![
        symbol("a", "src/a.rs", 1),
        symbol("b", "src/b.rs", 1),
        symbol("a", "src/z.rs", 9),
    ];
    let err = WikiSkeleton::new(vec![page]).unwrap_err();
    assert_eq!(
        err,
        SkeletonError::DuplicateSymbol {
            page: "c".into(),
            id: "a".into()
        }
    );
}

#[test]
fn the_schema_version_is_embedded_and_round_trips() {
    let skeleton = WikiSkeleton::new(vec![community_page(gaps())]).unwrap();
    let json = skeleton.to_json().unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["schema_version"], SCHEMA_VERSION);
    assert_eq!(WikiSkeleton::from_json(&json).unwrap(), skeleton);
}

#[test]
fn another_schema_version_is_refused_on_read() {
    let json = WikiSkeleton::new(vec![]).unwrap().to_json().unwrap();
    let older = json.replace(SCHEMA_VERSION, "ai-architect.wiki-skeleton/0");
    let err = WikiSkeleton::from_json(&older).unwrap_err();
    assert_eq!(
        err,
        SkeletonError::SchemaVersion {
            found: "ai-architect.wiki-skeleton/0".into()
        }
    );
    assert!(matches!(
        WikiSkeleton::from_json("{"),
        Err(SkeletonError::Json(_))
    ));
}

/// claude[bot] review on #475: another version is refused as `SchemaVersion`
/// even when its shape is not v1's, and the v1 shape is still strict.
#[test]
fn another_version_with_another_shape_is_still_a_schema_version_error() {
    let v2 = r#"{"schema_version":"ai-architect.wiki-skeleton/2","pages":"renamed","extra":1}"#;
    let err = WikiSkeleton::from_json(v2).unwrap_err();
    let found = "ai-architect.wiki-skeleton/2".to_owned();
    assert_eq!(err, SkeletonError::SchemaVersion { found });
    // Plain serde refuses it too (strict shape first, then the version).
    assert!(serde_json::from_str::<WikiSkeleton>(v2).is_err());
    let v2_v1_shape = r#"{"schema_version":"ai-architect.wiki-skeleton/2","pages":[]}"#;
    let serde_err = serde_json::from_str::<WikiSkeleton>(v2_v1_shape)
        .unwrap_err()
        .to_string();
    assert!(serde_err.contains("schema version"), "{serde_err}");
    let null_pages = format!(r#"{{"schema_version":"{SCHEMA_VERSION}","pages":null}}"#);
    let err = WikiSkeleton::from_json(&null_pages).unwrap_err();
    assert!(
        matches!(&err, SkeletonError::Json(m) if !m.contains("missing")),
        "{err:?}"
    );
    let extra = format!(r#"{{"schema_version":"{SCHEMA_VERSION}","pages":[],"extra":1}}"#);
    assert!(
        matches!(WikiSkeleton::from_json(&extra), Err(SkeletonError::Json(m)) if m.contains("extra"))
    );
    let missing = format!(r#"{{"schema_version":"{SCHEMA_VERSION}"}}"#);
    assert!(
        matches!(WikiSkeleton::from_json(&missing), Err(SkeletonError::Json(m)) if m.contains("pages"))
    );
    let unknown_page_field = format!(
        r#"{{"schema_version":"{SCHEMA_VERSION}","pages":[{{"kind":"process","id":"p","title":"t","members":[],"edges":[],"cross_community_edges":{{}},"source_files":[],"coverage":[],"x":1}}]}}"#
    );
    assert!(WikiSkeleton::from_json(&unknown_page_field).is_err());
}

/// Codex review on #475: the derived `Deserialize` must not bypass `new`.
#[test]
fn plain_serde_deserialisation_applies_the_same_checks_as_from_json() {
    let unsupported = r#"{"schema_version":"unsupported","pages":[]}"#;
    assert!(serde_json::from_str::<WikiSkeleton>(unsupported).is_err());
    let page = |id: &str| {
        format!(
            r#"{{"kind":"process","id":"{id}","title":"t","members":[],"edges":[],"cross_community_edges":{{}},"source_files":[],"coverage":[]}}"#
        )
    };
    let doc = |pages: &[String]| {
        format!(
            r#"{{"schema_version":"{SCHEMA_VERSION}","pages":[{}]}}"#,
            pages.join(",")
        )
    };
    let twice = doc(&[page("p"), page("p")]);
    assert!(serde_json::from_str::<WikiSkeleton>(&twice).is_err());
    assert!(serde_json::from_str::<WikiSkeleton>(&doc(&[page("")])).is_err());
    let unordered = doc(&[page("b"), page("a")]);
    let read: WikiSkeleton = serde_json::from_str(&unordered).unwrap();
    let order: Vec<&str> = read.pages().iter().map(|p| p.id.as_str()).collect();
    assert_eq!(order, ["a", "b"]);
    assert_eq!(read, WikiSkeleton::from_json(&unordered).unwrap());
}

#[test]
fn every_error_has_a_message() {
    let errors = [
        SkeletonError::EmptyFile,
        SkeletonError::ZeroLine,
        SkeletonError::EmptyId("page"),
        SkeletonError::DuplicatePage {
            kind: PageKind::Community,
            id: "c".into(),
        },
        SkeletonError::UnsafePath("/x".into()),
        SkeletonError::DuplicateEdge {
            page: "c".into(),
            source: "a".into(),
            target: "b".into(),
            relation: "Calls".into(),
        },
        SkeletonError::StaleSourceFiles { page: "c".into() },
        SkeletonError::DuplicateSymbol {
            page: "c".into(),
            id: "a".into(),
        },
        SkeletonError::SelfCrossCommunity { page: "c".into() },
        SkeletonError::SchemaVersion { found: "x".into() },
        SkeletonError::Json("bad".into()),
    ];
    for error in errors {
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn a_path_that_could_leave_the_repo_is_refused() {
    for bad in [
        "/etc/passwd",
        "../x",
        "src/../../x",
        "C:\\x",
        "src\\a.rs",
        "a\u{0}b",
        "a\nb",
    ] {
        let err = Location::new(bad, 1).unwrap_err();
        assert_eq!(err, SkeletonError::UnsafePath(bad.to_owned()), "{bad:?}");
    }
    for fine in ["src/a.rs", "a..b/c.rs", "./src/a.rs", ".hidden/x"] {
        assert!(Location::new(fine, 1).is_ok(), "{fine:?}");
    }
    let mut page = Page::new(PageKind::Community, "c", "x");
    page.coverage = vec![CoverageGap::ParseIncomplete {
        file: "/abs.rs".into(),
    }];
    let err = WikiSkeleton::new(vec![page]).unwrap_err();
    assert_eq!(err, SkeletonError::UnsafePath("/abs.rs".into()));
}

#[test]
fn one_edge_with_two_provenance_tags_is_refused_but_two_call_sites_stay() {
    let mut page = Page::new(PageKind::Community, "c", "x");
    page.edges = vec![
        Edge::new(
            ("a", "b"),
            "Calls",
            Provenance::Extracted,
            loc("src/a.rs", 4),
        ),
        Edge::new(
            ("a", "b"),
            "Calls",
            Provenance::Inferred,
            loc("src/a.rs", 4),
        ),
    ];
    let err = WikiSkeleton::new(vec![page.clone()]).unwrap_err();
    let expected = SkeletonError::DuplicateEdge {
        page: "c".into(),
        source: "a".into(),
        target: "b".into(),
        relation: "Calls".into(),
    };
    assert_eq!(err, expected);
    page.edges[1] = Edge::new(
        ("a", "b"),
        "Calls",
        Provenance::Inferred,
        loc("src/a.rs", 5),
    );
    assert_eq!(
        WikiSkeleton::new(vec![page]).unwrap().pages()[0]
            .edges
            .len(),
        2
    );
}

#[test]
fn edge_fields_are_readable() {
    let edge = Edge::new(
        ("a", "b"),
        "Calls",
        Provenance::Inferred,
        loc("src/a.rs", 4),
    );
    assert_eq!(
        (edge.source(), edge.target(), edge.relation()),
        ("a", "b", "Calls")
    );
    assert_eq!(
        (edge.provenance(), edge.location().line()),
        (Provenance::Inferred, 4)
    );
}

/// Every level refuses an unknown field, and the gap enum refuses an unknown
/// kind. Removing any one `deny_unknown_fields` fails exactly its row.
#[test]
fn an_unknown_field_is_refused_at_every_level() {
    let golden = include_str!("testdata/skeleton_v1.json");
    let rows = [
        ("location", r#""line": 3"#, r#""line": 3, "x": 1"#),
        (
            "symbol",
            r#""kind": "Function","#,
            r#""kind": "Function", "x": 1,"#,
        ),
        (
            "edge",
            r#""relation": "Calls","#,
            r#""relation": "Calls", "x": 1,"#,
        ),
        (
            "gap",
            r#""kind": "parse_incomplete","#,
            r#""kind": "parse_incomplete", "x": 1,"#,
        ),
        (
            "gap kind",
            r#""kind": "parse_incomplete""#,
            r#""kind": "unparsed""#,
        ),
        (
            "missed gap",
            r#""name": "unknown_fn","#,
            r#""name": "unknown_fn", "x": 1,"#,
        ),
        (
            "page",
            r#""title": "parser","#,
            r#""title": "parser", "x": 1,"#,
        ),
    ];
    for (level, from, to) in rows {
        let tampered = golden.replacen(from, to, 1);
        assert_ne!(tampered, golden, "{level}: the fixture must contain {from}");
        assert!(
            WikiSkeleton::from_json(&tampered).is_err(),
            "{level} accepted an unknown field"
        );
    }
}

#[test]
fn errors_from_new_stay_typed_on_the_read_path_and_location_errors_are_json() {
    let doc = |pages: &str| format!(r#"{{"schema_version":"{SCHEMA_VERSION}","pages":[{pages}]}}"#);
    let page = |kind: &str, id: &str, members: &str| {
        format!(
            r#"{{"kind":"{kind}","id":"{id}","title":"t","members":[{members}],"edges":[],"cross_community_edges":{{}},"source_files":[],"coverage":[]}}"#
        )
    };
    let two = doc(&format!(
        "{},{}",
        page("process", "p", ""),
        page("process", "p", "")
    ));
    let err = WikiSkeleton::from_json(&two).unwrap_err();
    assert_eq!(
        err,
        SkeletonError::DuplicatePage {
            kind: PageKind::Process,
            id: "p".into()
        }
    );
    let empty = doc(&page("process", "", ""));
    assert_eq!(
        WikiSkeleton::from_json(&empty),
        Err(SkeletonError::EmptyId("page"))
    );
    let zero = r#"{"id":"a","name":"a","kind":"Function","location":{"file":"src/a.rs","line":0}}"#;
    let err = WikiSkeleton::from_json(&doc(&page("process", "p", zero))).unwrap_err();
    assert!(
        matches!(&err, SkeletonError::Json(m) if m.contains("1-based")),
        "{err:?}"
    );
}

/// Words that name a network or model-client API, matched case-insensitively
/// and on identifier boundaries, so `hyperlink`, `philanthropic` and
/// `network` do not trip it while `OpenAI` and `{net::TcpListener}` do.
const FORBIDDEN: [&str; 14] = [
    "tcpstream",
    "tcplistener",
    "udpsocket",
    "reqwest",
    "ureq",
    "hyper",
    "anthropic",
    "openai",
    "net::",
    "::net",
    "process::command",
    "command::new",
    "std::process",
    "tokio::process",
];

/// Only alphanumerics glue: `_` is a boundary, so crate names such as
/// `hyper_util` or `async_openai` still match.
fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// `pattern` occurs in `text` with no identifier character glued to an
/// identifier end of the pattern.
fn names_on_a_boundary(text: &str, pattern: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let glued_before = pattern.starts_with(is_ident);
    let glued_after = pattern.ends_with(is_ident);
    lower.match_indices(pattern).any(|(i, _)| {
        let before = lower[..i].chars().next_back();
        let after = lower[i + pattern.len()..].chars().next();
        let glued_left = glued_before && before.is_some_and(is_ident);
        let glued_right = glued_after && after.is_some_and(is_ident);
        !glued_left && !glued_right
    })
}

fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_boundary_matcher_catches_disguised_names_and_spares_innocent_words() {
    let hits = |text: &str| FORBIDDEN.iter().any(|p| names_on_a_boundary(text, p));
    for bad in [
        "use hyper_util::client;",
        "async_openai::Client",
        "reqwest_middleware",
        "anthropic_sdk",
        "use std::process::{Command as C};",
        "use std::{net::TcpListener};",
        "OpenAI client",
        "std::process::Command",
        "Anthropic",
    ] {
        assert!(hits(bad), "missed {bad:?}");
    }
    for fine in [
        "a hyperlink",
        "philanthropic",
        "no network at index time",
        "magnet",
    ] {
        assert!(!hits(fine), "false positive on {fine:?}");
    }
}

/// A tripwire for criteria 1 and 2, not their enforcement: nothing under
/// `src/wiki/` (recursively, `*.rs`) may name a network or model-client API.
/// The real enforcement is the runtime no-egress test that lands with the
/// export path in a later slice of #196.
#[test]
fn the_wiki_module_names_no_network_or_model_client_api() {
    let mut sources = Vec::new();
    rust_sources(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/wiki"),
        &mut sources,
    );
    let wiki = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/wiki");
    sources.retain(|p| *p != wiki.join("skeleton_tests.rs"));
    assert!(
        sources.len() >= 2,
        "the tripwire must scan the wiki sources: {sources:?}"
    );
    for path in &sources {
        let text = std::fs::read_to_string(path).unwrap();
        for pattern in FORBIDDEN {
            assert!(
                !names_on_a_boundary(&text, pattern),
                "{} names {pattern}",
                path.display()
            );
        }
    }
}
