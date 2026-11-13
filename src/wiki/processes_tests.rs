use super::*;
use crate::graph_store::{cypher_str, PropEdge};
use crate::wiki::{Provenance, WikiSkeleton};

fn open_store() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("wiki_processes_")
        .tempdir()
        .expect("tempdir");
    let store = GraphStore::open_or_create(&dir.path().join("graph")).expect("open");
    store.create_schema().expect("schema");
    (dir, store)
}

fn node(id: &str, name: &str, line: u32) -> Vec<(String, String)> {
    vec![
        ("id".to_owned(), cypher_str(id)),
        ("name".to_owned(), cypher_str(name)),
        ("qualified_name".to_owned(), cypher_str(id)),
        ("start_line".to_owned(), line.to_string()),
        ("end_line".to_owned(), line.to_string()),
    ]
}

fn named(id: &str) -> Vec<(String, String)> {
    vec![
        ("id".into(), cypher_str(id)),
        ("name".into(), cypher_str(id)),
    ]
}

fn insert(store: &GraphStore, label: &str, rows: Vec<Vec<(String, String)>>) {
    store.bulk_insert_nodes(label, &rows).expect(label);
}

fn link(store: &GraphStore, rel: &str, pairs: &[(&str, &str)]) {
    let edges: Vec<PropEdge> = pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string(), Vec::new()))
        .collect();
    store.bulk_insert_edges(rel, &edges).expect(rel);
}

const C1: &str = "community::louvain::1::0";
const C2: &str = "community::louvain::1::1";
const PF: &str = "proc_f";
const PM: &str = "proc_m";

/// Two processes. `proc_f` (entry `f`): `f`, `g`, `S::m` and `weird::h` (no
/// file extension, so no file). `proc_m` (entry `S::m`): `S::m` and `g`.
/// Communities: c1 = {f, g}, c2 = {S::m}; `h` is in none. `out` is in no
/// process: its only participation, like that of `weird::lost` (no file),
/// points at a process with an empty id, so neither is listed or counted.
pub(in crate::wiki) fn fixture() -> (tempfile::TempDir, GraphStore) {
    let (dir, store) = open_store();
    insert(&store, "Community", vec![named(C1), named(C2)]);
    insert(&store, "Process", vec![named(PF), named(PM), named("")]);
    insert(
        &store,
        "Function",
        vec![
            node("src/a.rs::f", "f", 3),
            node("src/a.rs::g", "g", 7),
            node("weird::h", "h", 5),
            node("src/c.rs::out", "out", 1),
            node("weird::lost", "lost", 2),
        ],
    );
    insert(&store, "Method", vec![node("src/b.rs::S::m", "m", 4)]);
    link(
        &store,
        "MemberOf_Function_Community",
        &[("src/a.rs::f", C1), ("src/a.rs::g", C1)],
    );
    link(
        &store,
        "MemberOf_Method_Community",
        &[("src/b.rs::S::m", C2)],
    );
    link(
        &store,
        "ParticipatesIn_Function_Process",
        &[
            ("src/a.rs::f", PF),
            ("src/a.rs::g", PF),
            ("weird::h", PF),
            ("src/a.rs::g", PM),
            ("src/c.rs::out", ""),
            ("weird::lost", ""),
        ],
    );
    link(
        &store,
        "ParticipatesIn_Method_Process",
        &[("src/b.rs::S::m", PF), ("src/b.rs::S::m", PM)],
    );
    link(
        &store,
        "EntryPointOf_Function_Process",
        &[("src/a.rs::f", PF)],
    );
    link(
        &store,
        "EntryPointOf_Method_Process",
        &[("src/b.rs::S::m", PM)],
    );
    link(
        &store,
        "Calls_Function_Function",
        &[
            ("src/a.rs::f", "src/a.rs::g"),
            ("src/a.rs::g", "weird::h"),
            ("weird::h", "src/a.rs::f"),
            ("src/a.rs::f", "src/c.rs::out"),
            ("src/c.rs::out", "src/a.rs::f"),
            ("weird::lost", "src/c.rs::out"),
        ],
    );
    link(
        &store,
        "Calls_Function_Method",
        &[("src/a.rs::f", "src/b.rs::S::m")],
    );
    link(
        &store,
        "Calls_Method_Function",
        &[("src/b.rs::S::m", "src/a.rs::g")],
    );
    link(
        &store,
        "OBSERVED_CALLS_Method_Function",
        &[("src/b.rs::S::m", "src/a.rs::f")],
    );
    (dir, store)
}

fn page<'a>(built: &'a ProcessPages, id: &str) -> &'a Page {
    built.pages.iter().find(|p| p.id == id).expect(id)
}

type EdgeKey = (String, String, String, Provenance, String, u32);

fn edge_keys(page: &Page) -> Vec<EdgeKey> {
    let mut keys: Vec<_> = page
        .edges
        .iter()
        .map(|e| {
            (
                e.source().to_owned(),
                e.target().to_owned(),
                e.relation().to_owned(),
                e.provenance(),
                e.location().file().to_owned(),
                e.location().line(),
            )
        })
        .collect();
    keys.sort();
    keys
}

fn calls(source: &str, target: &str, file: &str, line: u32) -> EdgeKey {
    (
        source.to_owned(),
        target.to_owned(),
        "Calls".to_owned(),
        Provenance::Inferred,
        file.to_owned(),
        line,
    )
}

#[test]
fn one_page_per_process_with_its_anchored_participants() {
    let (_dir, store) = fixture();
    let built = process_pages(&store).expect("pages");
    let ids: Vec<&str> = built.pages.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, [PF, PM], "the process with an empty id has no page");
    let pf = page(&built, PF);
    assert_eq!(pf.kind, PageKind::Process);
    assert_eq!(pf.title, PF);
    let mut members: Vec<(&str, &str, &str, u32)> = pf
        .members
        .iter()
        .map(|s| {
            (
                s.id.as_str(),
                s.kind.as_str(),
                s.location.file(),
                s.location.line(),
            )
        })
        .collect();
    members.sort();
    assert_eq!(
        members,
        [
            ("src/a.rs::f", "Function", "src/a.rs", 3),
            ("src/a.rs::g", "Function", "src/a.rs", 7),
            ("src/b.rs::S::m", "Method", "src/b.rs", 4),
        ],
        "the entry point is a member; weird::h has no file"
    );
    let pm: BTreeSet<&str> = page(&built, PM)
        .members
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(pm, BTreeSet::from(["src/a.rs::g", "src/b.rs::S::m"]));
    assert!(built
        .pages
        .iter()
        .all(|p| p.members.iter().all(|s| s.id != "src/c.rs::out")));
}

#[test]
fn a_participant_with_no_file_is_reported_not_dropped() {
    let (_dir, store) = fixture();
    let built = process_pages(&store).expect("pages");
    assert_eq!(built.unanchored_members, ["weird::h"]);
    // `h -> f` in proc_f: its source has no anchor. `lost -> out` is in the
    // process with an empty id, which is no process.
    assert_eq!(built.unanchored_edges, 1);
    for page in &built.pages {
        assert!(page.edges.iter().all(|e| e.source() != "weird::h"));
    }
}

#[test]
fn edges_are_the_call_chain_between_participants_of_the_same_process() {
    let (_dir, store) = fixture();
    let built = process_pages(&store).expect("pages");
    assert_eq!(
        edge_keys(page(&built, PF)),
        [
            calls("src/a.rs::f", "src/a.rs::g", "src/a.rs", 3),
            calls("src/a.rs::f", "src/b.rs::S::m", "src/a.rs", 3),
            calls("src/a.rs::g", "weird::h", "src/a.rs", 7),
            calls("src/b.rs::S::m", "src/a.rs::g", "src/b.rs", 4),
        ],
        "f -> out and out -> f leave the process; OBSERVED_CALLS has no provenance"
    );
    assert_eq!(
        edge_keys(page(&built, PM)),
        [calls("src/b.rs::S::m", "src/a.rs::g", "src/b.rs", 4)],
        "f -> m is not on proc_m: f does not participate in it"
    );
}

#[test]
fn cross_community_counts_edges_between_two_clustered_ends_by_target_community() {
    let (_dir, store) = fixture();
    let built = process_pages(&store).expect("pages");
    // f -> m enters c2, m -> g enters c1; f -> g stays in c1; g -> h does
    // not cross: h is in no community.
    assert_eq!(
        page(&built, PF).cross_community_edges,
        BTreeMap::from([(C1.to_owned(), 1), (C2.to_owned(), 1)])
    );
    assert_eq!(
        page(&built, PM).cross_community_edges,
        BTreeMap::from([(C1.to_owned(), 1)])
    );
}

#[test]
fn a_store_with_no_process_has_no_page() {
    let (_dir, store) = open_store();
    let built = process_pages(&store).expect("pages");
    assert!(built.pages.is_empty());
    assert!(built.unanchored_members.is_empty());
    assert_eq!(built.unanchored_edges, 0);
}

#[test]
fn process_and_community_pages_share_one_valid_skeleton() {
    let (_dir, store) = fixture();
    let mut pages = process_pages(&store).expect("processes").pages;
    pages.extend(
        crate::wiki::community_pages(&store)
            .expect("communities")
            .pages,
    );
    let skeleton = WikiSkeleton::new(pages).expect("a valid skeleton");
    let kinds: Vec<PageKind> = skeleton.pages().iter().map(|p| p.kind).collect();
    assert_eq!(
        kinds,
        [
            PageKind::Community,
            PageKind::Community,
            PageKind::Process,
            PageKind::Process
        ]
    );
    let pf = skeleton
        .pages()
        .iter()
        .find(|p| p.kind == PageKind::Process && p.id == PF)
        .expect(PF);
    assert_eq!(pf.source_files(), ["src/a.rs", "src/b.rs"]);
    assert!(pf.coverage.is_empty());
}

/// The real pipeline: index, resolve, cluster, build. Every `Process` node
/// has a page, every anchored participant is on the page of each process it
/// participates in, the entry point is among them, and two builds of the
/// same graph serialise to the same bytes (criterion 6).
#[test]
fn pages_of_an_indexed_and_clustered_repo_form_a_deterministic_skeleton() {
    use crate::test_support::TempDirExt;
    let tmp = tempfile::Builder::new()
        .prefix("wiki_processes_e2e_")
        .tempdir()
        .expect("tempdir")
        .keep_managed();
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("main.rs"),
        "mod work;\nfn main() {\n    work::step_one();\n}\n",
    )
    .unwrap();
    std::fs::write(
        src.join("work.rs"),
        "pub fn step_one() {\n    step_two();\n}\n\
         pub fn step_two() {\n    step_three();\n}\n\
         pub fn step_three() {}\n",
    )
    .unwrap();
    let graph_dir = tmp.join("graph");
    crate::indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    crate::resolver::resolve_graph(&store).expect("resolve");
    crate::clustering::cluster_graph(&store, 1.0).expect("cluster");

    let built = process_pages(&store).expect("pages");
    let processes = store
        .execute_query("MATCH (p:Process) RETURN p.id, p.entry_point_id ORDER BY p.id")
        .unwrap();
    let page_ids: Vec<&str> = built.pages.iter().map(|p| p.id.as_str()).collect();
    let node_ids: Vec<&str> = processes.rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(page_ids, node_ids);
    assert!(!page_ids.is_empty(), "main is an entry point");

    for row in &processes.rows {
        let page = built.pages.iter().find(|p| p.id == row[0]).unwrap();
        let entry = &row[1];
        if !built.unanchored_members.contains(entry) {
            assert!(
                page.members.iter().any(|s| &s.id == entry),
                "entry point {entry} is a member of {}",
                page.id
            );
        }
    }
    // `main -> step_one -> step_two -> step_three` is one chain: its calls
    // are on the page of the process whose entry point is `main`.
    let main_page = built
        .pages
        .iter()
        .find(|p| p.members.iter().any(|s| s.name == "main"))
        .expect("a process enters at main");
    let names: BTreeSet<&str> = main_page.members.iter().map(|s| s.name.as_str()).collect();
    for name in ["main", "step_one", "step_two", "step_three"] {
        assert!(names.contains(name), "{name} in {names:?}");
    }
    let step = main_page
        .edges
        .iter()
        .find(|e| e.source().ends_with("step_two") && e.target().ends_with("step_three"))
        .expect("step_two -> step_three");
    assert_eq!(step.relation(), "Calls");
    assert_eq!(step.provenance(), Provenance::Inferred);
    assert_eq!(
        (step.location().file(), step.location().line()),
        ("work.rs", 4)
    );

    let first = WikiSkeleton::new(built.pages).unwrap().to_json().unwrap();
    let again = process_pages(&store).expect("pages again");
    let second = WikiSkeleton::new(again.pages).unwrap().to_json().unwrap();
    assert_eq!(first, second);
}
