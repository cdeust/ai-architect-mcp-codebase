use super::*;
use crate::graph_store::GraphStore;
use crate::indexer::coverage::FileCoverage;
use crate::wiki::{community_pages, process_pages, Location, PageKind, Symbol, WikiSkeleton};

fn member(id: &str, file: &str, line: u32) -> Symbol {
    Symbol {
        id: id.to_owned(),
        name: id.to_owned(),
        kind: "Function".to_owned(),
        location: Location::new(file, line).expect("location"),
    }
}

fn page(kind: PageKind, id: &str, members: Vec<Symbol>) -> Page {
    let mut page = Page::new(kind, id, id);
    page.members = members;
    page
}

fn gap(kind: CoverageKind) -> FileCoverage {
    FileCoverage {
        kind,
        detail: String::new(),
        error_ranges: if kind == CoverageKind::ParsePartial {
            vec![(2, 3)]
        } else {
            Vec::new()
        },
    }
}

/// `src/a.rs` and `src/lone.rs` parsed with errors, `src/b.rs` was skipped,
/// `src/c.rs` is clean (absent from the sparse record).
fn report() -> CoverageReport {
    let mut report = CoverageReport::new("full", 4);
    report
        .files
        .insert("src/a.rs".into(), gap(CoverageKind::ParsePartial));
    report
        .files
        .insert("src/lone.rs".into(), gap(CoverageKind::ParsePartial));
    report
        .files
        .insert("src/b.rs".into(), gap(CoverageKind::Skipped));
    report
}

fn files(page: &Page) -> Vec<&str> {
    page.coverage
        .iter()
        .map(|g| match g {
            CoverageGap::ParseIncomplete { file } => file.as_str(),
            CoverageGap::Missed { name, .. } => name.as_str(),
        })
        .collect()
}

/// A page with a member in a partial file carries the gap, once, whatever
/// the number of members there; a page with members only in clean or
/// skipped files carries none; a partial file no page lists is returned.
#[test]
fn a_partial_file_lands_on_every_page_with_a_member_in_it() {
    let mut pages = vec![
        page(
            PageKind::Community,
            "c1",
            vec![
                member("src/a.rs::f", "src/a.rs", 1),
                member("src/a.rs::g", "src/a.rs", 5),
            ],
        ),
        page(
            PageKind::Community,
            "c2",
            vec![
                member("src/c.rs::h", "src/c.rs", 1),
                member("src/b.rs::k", "src/b.rs", 1),
            ],
        ),
        page(
            PageKind::Process,
            "p1",
            vec![
                member("src/a.rs::g", "src/a.rs", 5),
                member("src/c.rs::h", "src/c.rs", 1),
            ],
        ),
    ];
    let attribution = attribute_parse_incomplete(&mut pages, &report());
    assert_eq!(files(&pages[0]), ["src/a.rs"]);
    assert!(
        pages[1].coverage.is_empty(),
        "skipped and clean files are no parse gap"
    );
    assert_eq!(files(&pages[2]), ["src/a.rs"]);
    assert_eq!(attribution.unattributed, ["src/lone.rs"]);
}

/// Running twice adds nothing, and a gap the page already carried is kept.
#[test]
fn attribution_is_idempotent_and_keeps_existing_gaps() {
    let missed = CoverageGap::Missed {
        name: "unknown_callee".into(),
        location: Location::new("src/a.rs", 2).unwrap(),
    };
    let mut pages = vec![page(
        PageKind::Community,
        "c1",
        vec![member("src/a.rs::f", "src/a.rs", 1)],
    )];
    pages[0].coverage.push(missed.clone());
    attribute_parse_incomplete(&mut pages, &report());
    let once = pages.clone();
    attribute_parse_incomplete(&mut pages, &report());
    assert_eq!(pages, once);
    assert_eq!(pages[0].coverage.len(), 2);
    assert!(pages[0].coverage.contains(&missed));
}

/// An empty record attributes nothing and leaves nothing unattributed.
#[test]
fn a_clean_record_attributes_nothing() {
    let mut pages = vec![page(
        PageKind::Community,
        "c1",
        vec![member("src/a.rs::f", "src/a.rs", 1)],
    )];
    let attribution = attribute_parse_incomplete(&mut pages, &CoverageReport::new("full", 1));
    assert!(pages[0].coverage.is_empty());
    assert!(attribution.unattributed.is_empty());
}

/// Criterion 5 in the artifact: the gap is serialised on the page it
/// belongs to, and the other page writes an explicit empty block.
#[test]
fn the_artifact_shows_the_gap_and_an_explicit_empty_block() {
    let mut pages = vec![
        page(
            PageKind::Community,
            "c1",
            vec![member("src/a.rs::f", "src/a.rs", 1)],
        ),
        page(
            PageKind::Community,
            "c2",
            vec![member("src/c.rs::h", "src/c.rs", 1)],
        ),
    ];
    attribute_parse_incomplete(&mut pages, &report());
    let json: serde_json::Value =
        serde_json::from_str(&WikiSkeleton::new(pages).unwrap().to_json().unwrap()).unwrap();
    assert_eq!(
        json["pages"][0]["coverage"],
        serde_json::json!([{ "kind": "parse_incomplete", "file": "src/a.rs" }])
    );
    assert_eq!(json["pages"][1]["coverage"], serde_json::json!([]));
}

/// The real pipeline: a Python repository with one file tree-sitter reads
/// with errors. Index, resolve, cluster, build the community and process
/// pages, attribute: every page with a member in the broken file carries
/// its gap, no other page does, and the artifact is the same on two runs.
#[test]
fn pages_of_an_indexed_repo_carry_the_parse_gap_of_their_files() {
    use crate::test_support::TempDirExt;
    let tmp = tempfile::Builder::new()
        .prefix("wiki_coverage_e2e_")
        .tempdir()
        .expect("tempdir")
        .keep_managed();
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("main.py"),
        "from work import step_one\n\ndef main():\n    step_one()\n",
    )
    .unwrap();
    std::fs::write(
        src.join("work.py"),
        "def step_one():\n    step_two()\n\ndef step_two():\n    return 1\n\n\
         def broken(  \n    @#$%^& }{\n",
    )
    .unwrap();
    let graph_dir = tmp.join("graph");
    let indexed = crate::indexer::index_codebase(&src, &graph_dir).expect("index");
    assert_eq!(
        indexed.coverage.files.get("work.py").map(|c| c.kind),
        Some(CoverageKind::ParsePartial),
        "the fixture must be parse_incomplete: {:?}",
        indexed.coverage.files
    );
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    crate::resolver::resolve_graph(&store).expect("resolve");
    crate::clustering::cluster_graph(&store, 1.0).expect("cluster");

    let build = || {
        let mut pages = community_pages(&store).expect("communities").pages;
        pages.extend(process_pages(&store).expect("processes").pages);
        let attribution = attribute_parse_incomplete(&mut pages, &indexed.coverage);
        (pages, attribution)
    };
    let (pages, attribution) = build();
    assert!(
        pages
            .iter()
            .any(|p| p.members.iter().any(|s| s.location.file() == "work.py")),
        "a page lists a member of work.py"
    );
    assert!(attribution.unattributed.is_empty());
    for page in &pages {
        let in_broken = page.members.iter().any(|s| s.location.file() == "work.py");
        let expected: Vec<&str> = if in_broken {
            vec!["work.py"]
        } else {
            Vec::new()
        };
        assert_eq!(files(page), expected, "{} {}", page.kind, page.id);
    }

    let first = WikiSkeleton::new(pages).unwrap().to_json().unwrap();
    let second = WikiSkeleton::new(build().0).unwrap().to_json().unwrap();
    assert_eq!(first, second);
}
