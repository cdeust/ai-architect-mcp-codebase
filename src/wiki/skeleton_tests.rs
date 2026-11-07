// Tests for wiki::skeleton — issue #196, criteria 3 to 7 (artifact slice).

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
fn source_files_are_derived_from_the_members() {
    let mut page = community_page(vec![]);
    page.source_files = vec!["stale.rs".to_owned()];
    let skeleton = WikiSkeleton::new(vec![page]).unwrap();
    assert_eq!(skeleton.pages()[0].source_files, ["src/a.rs", "src/b.rs"]);
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

/// Codex review on #475: the derived `Deserialize` must not bypass `new`.
#[test]
fn plain_serde_deserialisation_applies_the_same_checks_as_from_json() {
    let unsupported = r#"{"schema_version":"unsupported","pages":[]}"#;
    assert!(serde_json::from_str::<WikiSkeleton>(unsupported).is_err());
    let page = |id: &str| {
        format!(
            r#"{{"kind":"process","id":"{id}","title":"t","members":[],"edges":[],"cross_community_edges":{{}},"coverage":[]}}"#
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
        SkeletonError::SchemaVersion { found: "x".into() },
        SkeletonError::Json("bad".into()),
    ];
    for error in errors {
        assert!(!error.to_string().is_empty());
    }
}

/// Criteria 1 and 2, mechanically, for the code that exists today: nothing
/// under `src/wiki/` may name a network or model-client API. The export path
/// added by later slices of #196 lands under the same directory and is held
/// to the same guard.
#[test]
fn the_wiki_module_names_no_network_or_model_client_api() {
    const FORBIDDEN: [&str; 9] = [
        "std::net",
        "TcpStream",
        "UdpSocket",
        "reqwest",
        "ureq",
        "hyper",
        "tokio::net",
        "anthropic",
        "openai",
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/wiki");
    let this_file = "skeleton_tests.rs";
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|n| n == this_file) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for token in FORBIDDEN {
            assert!(!text.contains(token), "{} names {token}", path.display());
        }
        scanned += 1;
    }
    assert!(
        scanned >= 2,
        "the guard must scan the wiki sources, scanned {scanned}"
    );
}
