use super::*;
use crate::indexer::coverage::{CoverageKind, FileCoverage};
use crate::wiki::{CoverageGap, Location, PageKind};

fn empty_store() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("wiki_export_")
        .tempdir()
        .expect("tempdir");
    let store = GraphStore::open_or_create(&dir.path().join("graph")).expect("open");
    store.create_schema().expect("schema");
    (dir, store)
}

/// A graph with nothing clustered exports a valid, empty skeleton: no page,
/// no fence, nothing unplaced, and the version embedded.
#[test]
fn an_empty_graph_exports_an_empty_versioned_skeleton() {
    let (_dir, store) = empty_store();
    let out = export(&store, &CoverageReport::new("full", 0)).expect("export");
    assert!(out.skeleton.pages().is_empty());
    assert_eq!(out.report, ExportReport::default());
    let json = out.skeleton.to_json().unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["schema_version"], crate::wiki::SCHEMA_VERSION);
    assert_eq!(value["pages"], serde_json::json!([]));
}

/// A `parse_incomplete` file of the record that no page lists is reported,
/// not dropped, even when the graph has no page at all.
#[test]
fn a_gap_with_no_page_is_reported_beside_the_skeleton() {
    let (_dir, store) = empty_store();
    let mut coverage = CoverageReport::new("full", 1);
    coverage.files.insert(
        "lost.py".into(),
        FileCoverage {
            kind: CoverageKind::ParsePartial,
            detail: String::new(),
            error_ranges: vec![(1, 2)],
        },
    );
    let out = export(&store, &coverage).expect("export");
    assert!(out.skeleton.pages().is_empty());
    assert_eq!(out.report.parse_incomplete_unattributed, vec!["lost.py"]);
}

const MAIN_PY: &str = "from work import hub, step_one\n\n\
def a():\n    hub()\n\n\
def b():\n    hub()\n\n\
def c():\n    hub()\n\n\
def d():\n    hub()\n\n\
def main():\n    step_one()\n    vanished_helper()\n";

const WORK_PY: &str = "def hub():\n    return 1\n\n\
def step_one():\n    return hub()\n\n\
def broken(  \n    @#$%^& }{\n";

/// A clean component of its own: its community page has no gap.
const CLEAN_PY: &str = "def left():\n    return right()\n\ndef right():\n    return 2\n";

/// The real pipeline: index a Python repository with a hub called from five
/// places, an unresolved call, a file tree-sitter reads with errors and a
/// clean file that calls nothing outside itself;
/// resolve, cluster, export. The skeleton holds every page the three
/// builders make, both kinds of gap land on the pages that list the symbol
/// they belong to, every page writes its coverage block, and two exports of
/// the same store are byte-identical and read back to the same skeleton.
#[test]
fn an_indexed_repo_exports_every_page_with_its_gaps() {
    use crate::test_support::TempDirExt;
    let tmp = tempfile::Builder::new()
        .prefix("wiki_export_e2e_")
        .tempdir()
        .expect("tempdir")
        .keep_managed();
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.py"), MAIN_PY).unwrap();
    std::fs::write(src.join("work.py"), WORK_PY).unwrap();
    std::fs::write(src.join("clean.py"), CLEAN_PY).unwrap();
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

    let out = export(&store, &indexed.coverage).expect("export");
    let pages = out.skeleton.pages();

    // Every page of every builder is in the skeleton, and nothing else.
    let mut built: Vec<(PageKind, String)> = Vec::new();
    for p in community_pages(&store).unwrap().pages {
        built.push((p.kind, p.id));
    }
    for p in process_pages(&store).unwrap().pages {
        built.push((p.kind, p.id));
    }
    for p in god_node_pages(&store).unwrap().pages {
        built.push((p.kind, p.id));
    }
    built.sort();
    let exported: Vec<(PageKind, String)> = pages.iter().map(|p| (p.kind, p.id.clone())).collect();
    assert_eq!(exported, built);
    for kind in [PageKind::Community, PageKind::Process, PageKind::GodNode] {
        assert!(
            pages.iter().any(|p| p.kind == kind),
            "no {kind} page: {pages:#?}"
        );
    }
    assert!(
        pages
            .iter()
            .any(|p| p.kind == PageKind::GodNode && p.title == "hub"),
        "hub is a god node: {:?} {pages:#?}",
        out.report.god_node_fence
    );
    assert!(out.report.god_node_fence.is_some());

    // The parse gap is on every page with a member in work.py, the open
    // site on every page that lists main, and on no other page.
    let parse_gap = CoverageGap::ParseIncomplete {
        file: "work.py".to_owned(),
    };
    let missed_gap = CoverageGap::Missed {
        name: "vanished_helper".to_owned(),
        location: Location::new("main.py", 17).unwrap(),
    };
    let (mut parse_carriers, mut missed_carriers) = (0, 0);
    for page in pages {
        let in_broken = page.members.iter().any(|s| s.location.file() == "work.py");
        let lists_main = page.members.iter().any(|s| s.name == "main");
        let label = format!("{} {}", page.kind, page.id);
        assert_eq!(page.coverage.contains(&parse_gap), in_broken, "{label}");
        assert_eq!(page.coverage.contains(&missed_gap), lists_main, "{label}");
        parse_carriers += usize::from(in_broken);
        missed_carriers += usize::from(lists_main);
    }
    assert!(parse_carriers > 0 && missed_carriers > 0, "{pages:#?}");
    assert!(out.report.parse_incomplete_unattributed.is_empty());
    assert!(out.report.missed_unanchored.is_empty());

    // Criterion 5 on the artifact: every page writes its block, present
    // when empty, and at least one page has an empty one.
    let json = out.skeleton.to_json().unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let blocks: Vec<&serde_json::Value> = value["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["coverage"])
        .collect();
    assert_eq!(blocks.len(), pages.len());
    assert!(blocks.iter().all(|b| b.is_array()), "{json}");
    assert!(
        blocks.iter().any(|b| b.as_array().unwrap().is_empty()),
        "{json}"
    );

    // Criterion 6: the same store gives the same bytes, and the artifact
    // reads back to the same skeleton.
    let again = export(&store, &indexed.coverage).expect("export again");
    assert_eq!(again.skeleton.to_json().unwrap(), json);
    assert_eq!(again.report, out.report);
    assert_eq!(WikiSkeleton::from_json(&json).unwrap(), out.skeleton);
}

/// `report` carries each list exactly as its builder or attribution returns
/// it, on the same store.
fn assert_report_is_the_builders(store: &GraphStore, coverage: &CoverageReport) -> ExportReport {
    let out = export(store, coverage).expect("export");
    let communities = community_pages(store).unwrap();
    let processes = process_pages(store).unwrap();
    let god_nodes = god_node_pages(store).unwrap();
    let mut pages = communities.pages.clone();
    pages.extend(processes.pages.clone());
    pages.extend(god_nodes.pages.clone());
    let partial = crate::wiki::attribute_parse_incomplete(&mut pages, coverage);
    let missed = crate::wiki::attribute_missed(&mut pages, store).unwrap();
    let expected = ExportReport {
        community_unanchored_members: communities.unanchored_members,
        community_unanchored_edges: communities.unanchored_edges,
        process_unanchored_members: processes.unanchored_members,
        process_unanchored_edges: processes.unanchored_edges,
        god_node_unanchored_members: god_nodes.unanchored_members,
        god_node_unanchored_edges: god_nodes.unanchored_edges,
        god_node_fence: god_nodes.fence,
        parse_incomplete_unattributed: partial.unattributed,
        missed_unattributed: missed.unattributed,
        missed_unanchored: missed.unanchored,
    };
    assert_eq!(out.report, expected);
    assert_eq!(out.skeleton, WikiSkeleton::new(pages).unwrap());
    out.report
}

/// On the fixtures of the builders and of the missed attribution, each with
/// something it cannot anchor or place, the report carries it.
#[test]
fn what_the_skeleton_cannot_hold_is_reported_as_each_builder_returns_it() {
    let (_c, communities) = crate::wiki::communities::tests::fixture();
    let report = assert_report_is_the_builders(&communities, &CoverageReport::new("full", 3));
    assert!(!report.community_unanchored_members.is_empty());
    assert!(report.community_unanchored_edges > 0);

    let (_m, missed) = crate::wiki::missed::tests::fixture();
    let report = assert_report_is_the_builders(&missed, &CoverageReport::new("full", 3));
    assert!(!report.missed_unattributed.is_empty());
    assert!(!report.missed_unanchored.is_empty());

    let (_p, processes) = crate::wiki::processes::tests::fixture();
    let report = assert_report_is_the_builders(&processes, &CoverageReport::new("full", 3));
    assert!(!report.process_unanchored_members.is_empty());
    assert!(report.process_unanchored_edges > 0);

    let (_s, star) = crate::wiki::god_nodes::tests::star();
    let report = assert_report_is_the_builders(&star, &CoverageReport::new("full", 2));
    assert!(report.god_node_unanchored_edges > 0);

    let (_h, hub) = crate::wiki::god_nodes::tests::unanchored_hub();
    let report = assert_report_is_the_builders(&hub, &CoverageReport::new("full", 2));
    assert!(!report.god_node_unanchored_members.is_empty());
}
