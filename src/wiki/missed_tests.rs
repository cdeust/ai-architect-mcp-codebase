use super::*;
use crate::graph_store::{cypher_str, PropEdge};
use crate::wiki::{community_pages, process_pages, Location, PageKind, Symbol, WikiSkeleton};

fn open_store() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("wiki_missed_")
        .tempdir()
        .expect("tempdir");
    let store = GraphStore::open_or_create(&dir.path().join("graph")).expect("open");
    store.create_schema().expect("schema");
    (dir, store)
}

fn symbol(id: &str, line: u32) -> Vec<(String, String)> {
    vec![
        ("id".to_owned(), cypher_str(id)),
        ("name".to_owned(), cypher_str(id)),
        ("qualified_name".to_owned(), cypher_str(id)),
        ("start_line".to_owned(), line.to_string()),
        ("end_line".to_owned(), line.to_string()),
    ]
}

/// A call site row: `line` 0 is what the store holds for "no line".
fn site(id: &str, callee: &str, line: u32, resolved: bool) -> Vec<(String, String)> {
    vec![
        ("id".to_owned(), cypher_str(id)),
        ("callee_name".to_owned(), cypher_str(callee)),
        ("line".to_owned(), line.to_string()),
        ("is_resolved".to_owned(), resolved.to_string()),
    ]
}

fn link(store: &GraphStore, rel: &str, pairs: &[(&str, &str)]) {
    let edges: Vec<PropEdge> = pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string(), Vec::new()))
        .collect();
    store.bulk_insert_edges(rel, &edges).expect(rel);
}

/// `f` (src/a.rs) holds an open site to `ghost` at line 4 and a resolved
/// one to `g`; method `S::m` (src/b.rs) holds an open site to `phantom` at
/// line 9; `orphan` (src/c.rs) holds an open site on no page; a module-level
/// open site has no definer; one open site has no line, one no callee name.
fn fixture() -> (tempfile::TempDir, GraphStore) {
    let (dir, store) = open_store();
    store
        .bulk_insert_nodes(
            "Function",
            &[
                symbol("src/a.rs::f", 3),
                symbol("src/a.rs::g", 7),
                symbol("src/c.rs::orphan", 1),
            ],
        )
        .expect("functions");
    store
        .bulk_insert_nodes("Method", &[symbol("src/b.rs::S::m", 8)])
        .expect("methods");
    store
        .bulk_insert_nodes(
            "CallSite",
            &[
                site("src/a.rs::f::call::ghost::4", "ghost", 4, false),
                site("src/a.rs::f::call::g::5", "g", 5, true),
                site("src/b.rs::S::m::call::phantom::9", "phantom", 9, false),
                site("src/c.rs::orphan::call::lost::2", "lost", 2, false),
                site("src/d.rs::call::top::1", "top", 1, false),
                site("src/a.rs::f::call::noline::0", "noline", 0, false),
                site("src/a.rs::f::call::anon::6", "", 6, false),
            ],
        )
        .expect("call sites");
    link(
        &store,
        "Defines_Function_CallSite",
        &[
            ("src/a.rs::f", "src/a.rs::f::call::ghost::4"),
            ("src/a.rs::f", "src/a.rs::f::call::g::5"),
            ("src/a.rs::f", "src/a.rs::f::call::noline::0"),
            ("src/a.rs::f", "src/a.rs::f::call::anon::6"),
            ("src/c.rs::orphan", "src/c.rs::orphan::call::lost::2"),
        ],
    );
    link(
        &store,
        "Defines_Method_CallSite",
        &[("src/b.rs::S::m", "src/b.rs::S::m::call::phantom::9")],
    );
    (dir, store)
}

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

fn missed(name: &str, file: &str, line: u32) -> CoverageGap {
    CoverageGap::Missed {
        name: name.to_owned(),
        location: Location::new(file, line).expect("location"),
    }
}

/// c1 lists `f` and `g`, c2 lists `S::m`, p1 lists `f` again, c3 lists
/// only `g` (whose body holds no open site).
fn pages() -> Vec<Page> {
    vec![
        page(
            PageKind::Community,
            "c1",
            vec![
                member("src/a.rs::f", "src/a.rs", 3),
                member("src/a.rs::g", "src/a.rs", 7),
            ],
        ),
        page(
            PageKind::Community,
            "c2",
            vec![member("src/b.rs::S::m", "src/b.rs", 8)],
        ),
        page(
            PageKind::Process,
            "p1",
            vec![member("src/a.rs::f", "src/a.rs", 3)],
        ),
        page(
            PageKind::Community,
            "c3",
            vec![member("src/a.rs::g", "src/a.rs", 7)],
        ),
    ]
}

/// An open site lands, at its own `file:line` and with its callee's
/// spelling, on every page that lists its definer, Function or Method; a
/// resolved site lands nowhere; a page whose members hold no open site
/// carries none.
#[test]
fn an_open_site_lands_on_every_page_that_lists_its_definer() {
    let (_dir, store) = fixture();
    let mut pages = pages();
    attribute_missed(&mut pages, &store).expect("attribute");
    assert_eq!(pages[0].coverage, [missed("ghost", "src/a.rs", 4)]);
    assert_eq!(pages[1].coverage, [missed("phantom", "src/b.rs", 9)]);
    assert_eq!(pages[2].coverage, [missed("ghost", "src/a.rs", 4)]);
    assert!(pages[3].coverage.is_empty(), "{:?}", pages[3].coverage);
}

/// No open site is dropped: a site whose definer is on no page, or that has
/// no definer, is unattributed; a site with no line or no callee name is
/// unanchored, and is not attributed even though its definer is on a page.
#[test]
fn a_site_off_every_page_or_with_no_line_is_reported_not_dropped() {
    let (_dir, store) = fixture();
    let mut pages = pages();
    let attribution = attribute_missed(&mut pages, &store).expect("attribute");
    assert_eq!(
        attribution.unattributed,
        ["src/c.rs::orphan::call::lost::2", "src/d.rs::call::top::1"]
    );
    assert_eq!(
        attribution.unanchored,
        ["src/a.rs::f::call::anon::6", "src/a.rs::f::call::noline::0"]
    );
    assert!(pages.iter().all(|p| !p.coverage.iter().any(
        |g| matches!(g, CoverageGap::Missed { name, .. } if name == "noline" || name.is_empty())
    )));
}

/// Running twice adds nothing, and a gap the page already carried is kept.
#[test]
fn attribution_is_idempotent_and_keeps_existing_gaps() {
    let (_dir, store) = fixture();
    let parse = CoverageGap::ParseIncomplete {
        file: "src/a.rs".into(),
    };
    let mut pages = pages();
    pages[0].coverage.push(parse.clone());
    attribute_missed(&mut pages, &store).expect("first");
    let once = pages.clone();
    attribute_missed(&mut pages, &store).expect("second");
    assert_eq!(pages, once);
    assert_eq!(pages[0].coverage, [parse, missed("ghost", "src/a.rs", 4)]);
}

/// A store with no open site attributes nothing and reports nothing.
#[test]
fn a_store_with_no_open_site_attributes_nothing() {
    let (_dir, store) = open_store();
    let mut pages = pages();
    let attribution = attribute_missed(&mut pages, &store).expect("attribute");
    assert!(pages.iter().all(|p| p.coverage.is_empty()));
    assert_eq!(attribution, MissedAttribution::default());
}

/// Criterion 5 in the artifact: the gap is serialised on its page with its
/// `file:line`, and a page with no gap writes an explicit empty block.
#[test]
fn the_artifact_shows_the_missed_gap_and_an_explicit_empty_block() {
    let (_dir, store) = fixture();
    let mut pages = pages();
    pages.truncate(2);
    pages.push(page(
        PageKind::Community,
        "c3",
        vec![member("src/a.rs::g", "src/a.rs", 7)],
    ));
    attribute_missed(&mut pages, &store).expect("attribute");
    let json: serde_json::Value =
        serde_json::from_str(&WikiSkeleton::new(pages).unwrap().to_json().unwrap()).unwrap();
    let page = |id: &str| {
        json["pages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap_or_else(|| panic!("page {id}"))
            .clone()
    };
    assert_eq!(
        page("c1")["coverage"],
        serde_json::json!([{
            "kind": "missed",
            "name": "ghost",
            "location": { "file": "src/a.rs", "line": 4 }
        }])
    );
    assert_eq!(page("c3")["coverage"], serde_json::json!([]));
}

/// The real pipeline: a Python repository whose `step_two` calls a name
/// defined nowhere. Index, resolve, cluster, build the community and
/// process pages, attribute: the open site is on every page that lists
/// `step_two`, at the line of the call, on no other page, and the artifact
/// is the same on two runs.
#[test]
fn pages_of_an_indexed_repo_carry_the_open_sites_of_their_members() {
    use crate::test_support::TempDirExt;
    let tmp = tempfile::Builder::new()
        .prefix("wiki_missed_e2e_")
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
        "def step_one():\n    step_two()\n\ndef step_two():\n    vanished_helper()\n    return 1\n",
    )
    .unwrap();
    let graph_dir = tmp.join("graph");
    crate::indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    crate::resolver::resolve_graph(&store).expect("resolve");
    crate::clustering::cluster_graph(&store, 1.0).expect("cluster");

    let build = || {
        let mut pages = community_pages(&store).expect("communities").pages;
        pages.extend(process_pages(&store).expect("processes").pages);
        let attribution = attribute_missed(&mut pages, &store).expect("attribute");
        (pages, attribution)
    };
    let (pages, attribution) = build();
    let expected = missed("vanished_helper", "work.py", 5);
    let mut carriers = 0;
    for page in &pages {
        let lists_definer = page.members.iter().any(|s| s.name == "step_two");
        let carries = page.coverage.contains(&expected);
        assert_eq!(carries, lists_definer, "{} {}", page.kind, page.id);
        carriers += usize::from(carries);
    }
    assert!(carriers > 0, "a page lists step_two: {pages:#?}");
    assert!(
        attribution.unanchored.is_empty(),
        "{:?}",
        attribution.unanchored
    );

    let first = WikiSkeleton::new(pages).unwrap().to_json().unwrap();
    let second = WikiSkeleton::new(build().0).unwrap().to_json().unwrap();
    assert_eq!(first, second);
}
