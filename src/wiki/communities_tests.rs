use super::*;
use crate::graph_store::cypher_str;
use crate::wiki::WikiSkeleton;

fn open_store() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("wiki_communities_")
        .tempdir()
        .expect("tempdir");
    let store = GraphStore::open_or_create(&dir.path().join("graph")).expect("open");
    store.create_schema().expect("schema");
    (dir, store)
}

fn node(id: &str, name: &str, line: Option<u32>) -> Vec<(String, String)> {
    let mut row = vec![
        ("id".to_owned(), cypher_str(id)),
        ("name".to_owned(), cypher_str(name)),
        ("qualified_name".to_owned(), cypher_str(id)),
    ];
    if let Some(line) = line {
        row.push(("start_line".to_owned(), line.to_string()));
        row.push(("end_line".to_owned(), line.to_string()));
    }
    row
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

use crate::graph_store::PropEdge;

const C1: &str = "community::louvain::1::0";
const C2: &str = "community::louvain::1::1";

/// Two communities. c1: `f`, `g` and `weird::h` (no file extension, so no
/// file). c2: `S`, `S::m` and module `modb` (no line). `orphan` is in no
/// community. `S::x` is a field, a label with no community.
pub(in crate::wiki) fn fixture() -> (tempfile::TempDir, GraphStore) {
    let (dir, store) = open_store();
    insert(
        &store,
        "Community",
        vec![
            vec![
                ("id".into(), cypher_str(C1)),
                ("name".into(), cypher_str("community_0")),
            ],
            vec![
                ("id".into(), cypher_str(C2)),
                ("name".into(), cypher_str("community_1")),
            ],
        ],
    );
    insert(
        &store,
        "Function",
        vec![
            node("src/a.rs::f", "f", Some(3)),
            node("src/a.rs::g", "g", Some(7)),
            node("weird::h", "h", Some(5)),
            node("src/c.rs::orphan", "orphan", Some(1)),
        ],
    );
    insert(&store, "Struct", vec![node("src/b.rs::S", "S", Some(2))]);
    insert(&store, "Method", vec![node("src/b.rs::S::m", "m", Some(4))]);
    insert(&store, "Module", vec![node("src/b.rs::modb", "modb", None)]);
    insert(
        &store,
        "Field",
        vec![vec![
            ("id".into(), cypher_str("src/b.rs::S::x")),
            ("name".into(), cypher_str("x")),
            ("start_line".into(), "2".into()),
        ]],
    );
    link(
        &store,
        "MemberOf_Function_Community",
        &[("src/a.rs::f", C1), ("src/a.rs::g", C1), ("weird::h", C1)],
    );
    link(&store, "MemberOf_Struct_Community", &[("src/b.rs::S", C2)]);
    link(
        &store,
        "MemberOf_Method_Community",
        &[("src/b.rs::S::m", C2)],
    );
    link(
        &store,
        "MemberOf_Module_Community",
        &[("src/b.rs::modb", C2)],
    );
    link(
        &store,
        "Calls_Function_Function",
        &[
            ("src/a.rs::f", "src/a.rs::g"),
            ("weird::h", "src/a.rs::f"),
            ("src/c.rs::orphan", "src/a.rs::f"),
        ],
    );
    link(
        &store,
        "Calls_Function_Method",
        &[("src/a.rs::f", "src/b.rs::S::m")],
    );
    link(
        &store,
        "Uses_Function_Struct",
        &[("src/a.rs::f", "src/b.rs::S")],
    );
    // A field is on no page: an edge to it is not a symbol-to-symbol edge.
    link(
        &store,
        "HasField_Struct_Field",
        &[("src/b.rs::S", "src/b.rs::S::x")],
    );
    link(
        &store,
        "HasMethod_Struct_Method",
        &[("src/b.rs::S", "src/b.rs::S::m")],
    );
    link(
        &store,
        "Defines_Module_Struct",
        &[("src/b.rs::modb", "src/b.rs::S")],
    );
    link(
        &store,
        "Calls_Method_Function",
        &[("src/b.rs::S::m", "src/a.rs::g")],
    );
    link(
        &store,
        "OBSERVED_CALLS_Function_Method",
        &[("src/a.rs::f", "src/b.rs::S::m")],
    );
    (dir, store)
}

fn page<'a>(built: &'a CommunityPages, id: &str) -> &'a Page {
    built.pages.iter().find(|p| p.id == id).expect(id)
}

fn edge_keys(page: &Page) -> Vec<(String, String, String, Provenance, String, u32)> {
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

#[test]
fn one_page_per_community_with_its_anchored_members() {
    let (_dir, store) = fixture();
    let built = community_pages(&store).expect("pages");
    let ids: Vec<&str> = built.pages.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, [C1, C2]);
    let c1 = page(&built, C1);
    assert_eq!(c1.kind, PageKind::Community);
    assert_eq!(c1.title, "community_0");
    let mut members: Vec<(String, String, String, u32)> = c1
        .members
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                s.kind.clone(),
                s.location.file().to_owned(),
                s.location.line(),
            )
        })
        .collect();
    members.sort();
    assert_eq!(
        members,
        [
            (
                "src/a.rs::f".into(),
                "Function".into(),
                "src/a.rs".into(),
                3
            ),
            (
                "src/a.rs::g".into(),
                "Function".into(),
                "src/a.rs".into(),
                7
            ),
        ]
    );
    let c2_kinds: BTreeSet<&str> = page(&built, C2)
        .members
        .iter()
        .map(|s| s.kind.as_str())
        .collect();
    assert_eq!(c2_kinds, BTreeSet::from(["Method", "Struct"]));
}

#[test]
fn a_member_with_no_file_or_no_line_is_reported_not_dropped() {
    let (_dir, store) = fixture();
    let built = community_pages(&store).expect("pages");
    assert_eq!(built.unanchored_members, ["src/b.rs::modb", "weird::h"]);
    // `modb -> S` (Defines) and `h -> f` (Calls): their source has no anchor.
    assert_eq!(built.unanchored_edges, 2);
    for page in &built.pages {
        assert!(page
            .members
            .iter()
            .all(|s| !built.unanchored_members.contains(&s.id)));
        assert!(page
            .edges
            .iter()
            .all(|e| !built.unanchored_members.contains(&e.source().to_owned())));
    }
}

#[test]
fn edges_sit_on_their_source_page_tagged_by_table_class_and_anchored_at_the_source() {
    let (_dir, store) = fixture();
    let built = community_pages(&store).expect("pages");
    let inferred = Provenance::Inferred;
    let extracted = Provenance::Extracted;
    let s = |v: &str| v.to_owned();
    assert_eq!(
        edge_keys(page(&built, C1)),
        [
            (
                s("src/a.rs::f"),
                s("src/a.rs::g"),
                s("Calls"),
                inferred,
                s("src/a.rs"),
                3
            ),
            (
                s("src/a.rs::f"),
                s("src/b.rs::S"),
                s("Uses"),
                inferred,
                s("src/a.rs"),
                3
            ),
            (
                s("src/a.rs::f"),
                s("src/b.rs::S::m"),
                s("Calls"),
                inferred,
                s("src/a.rs"),
                3
            ),
        ],
        "orphan -> f is on no page (orphan has none), OBSERVED_CALLS has no provenance"
    );
    assert_eq!(
        edge_keys(page(&built, C2)),
        [
            (
                s("src/b.rs::S"),
                s("src/b.rs::S::m"),
                s("HasMethod"),
                extracted,
                s("src/b.rs"),
                2
            ),
            (
                s("src/b.rs::S::m"),
                s("src/a.rs::g"),
                s("Calls"),
                inferred,
                s("src/b.rs"),
                4
            ),
        ]
    );
}

#[test]
fn cross_community_counts_distinct_edges_to_other_communities_only() {
    let (_dir, store) = fixture();
    let built = community_pages(&store).expect("pages");
    // f -> m (Calls) and f -> S (Uses) leave c1; f -> g and h -> f stay.
    assert_eq!(
        page(&built, C1).cross_community_edges,
        BTreeMap::from([(C2.to_owned(), 2)])
    );
    // m -> g leaves c2; S -> m and modb -> S stay.
    assert_eq!(
        page(&built, C2).cross_community_edges,
        BTreeMap::from([(C1.to_owned(), 1)])
    );
}

#[test]
fn provenance_follows_the_table_class_and_runtime_traces_have_none() {
    assert_eq!(
        provenance_of("HasMethod_Struct_Method"),
        Some(Provenance::Extracted)
    );
    assert_eq!(
        provenance_of("Defines_Module_Struct"),
        Some(Provenance::Extracted)
    );
    for rel in [
        "Calls_Function_Function",
        "Uses_Method_Trait",
        "Imports_Module_Function",
        "Implements_Struct_Trait",
        "Extends_Trait_Trait",
    ] {
        assert_eq!(provenance_of(rel), Some(Provenance::Inferred), "{rel}");
    }
    assert_eq!(provenance_of("OBSERVED_CALLS_Function_Method"), None);
    assert_eq!(relation_of("HasMethod_Struct_Method"), "HasMethod");
}

#[test]
fn anchor_needs_a_known_file_prefix_and_a_positive_line() {
    assert_eq!(
        anchor("src/a.rs::f", "3"),
        Location::new("src/a.rs", 3).ok()
    );
    assert_eq!(anchor("src/a.rs::f", ""), None);
    assert_eq!(anchor("src/a.rs::f", "0"), None);
    assert_eq!(anchor("src/a.rs::f", "-1"), None);
    assert_eq!(anchor("weird::h", "3"), None);
}

#[test]
fn a_store_with_no_community_has_no_page() {
    let (_dir, store) = open_store();
    let built = community_pages(&store).expect("pages");
    assert!(built.pages.is_empty());
    assert!(built.unanchored_members.is_empty());
    assert_eq!(built.unanchored_edges, 0);
}

/// The real pipeline: index, resolve, cluster, build. Every page is a
/// `Community` node, every anchored member of `collect_cluster_memberships`
/// is on exactly the page of its community, the pages form a valid skeleton,
/// and two builds of the same graph serialise to the same bytes (criterion 6).
#[test]
fn pages_of_an_indexed_and_clustered_repo_form_a_deterministic_skeleton() {
    use crate::test_support::TempDirExt;
    let tmp = tempfile::Builder::new()
        .prefix("wiki_communities_e2e_")
        .tempdir()
        .expect("tempdir")
        .keep_managed();
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..4 {
        std::fs::write(
            src.join(format!("m{i}.rs")),
            format!(
                "pub struct S{i} {{ pub x: i32 }}\n\
                 impl S{i} {{\n    pub fn run(&self) {{ helper{i}(); }}\n}}\n\
                 pub fn helper{i}() {{}}\n"
            ),
        )
        .unwrap();
    }
    let graph_dir = tmp.join("graph");
    crate::indexer::index_codebase(&src, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    crate::resolver::resolve_graph(&store).expect("resolve");
    crate::clustering::cluster_graph(&store, 1.0).expect("cluster");

    let built = community_pages(&store).expect("pages");
    let communities = store
        .execute_query("MATCH (c:Community) RETURN c.id ORDER BY c.id")
        .unwrap();
    let page_ids: Vec<&str> = built.pages.iter().map(|p| p.id.as_str()).collect();
    let node_ids: Vec<&str> = communities.rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(page_ids, node_ids);
    assert!(!page_ids.is_empty());

    let memberships = crate::clustering::collect_cluster_memberships(&store).unwrap();
    let mut listed = 0;
    for m in &memberships.entries {
        let on: Vec<&str> = built
            .pages
            .iter()
            .filter(|p| p.members.iter().any(|s| s.id == m.qualified_name))
            .map(|p| p.id.as_str())
            .collect();
        if built.unanchored_members.contains(&m.qualified_name) {
            assert!(on.is_empty(), "{}", m.qualified_name);
        } else {
            assert_eq!(on, [m.community_id.as_str()], "{}", m.qualified_name);
            listed += 1;
        }
    }
    assert!(listed > 0);
    // `helper0` is on line 5 of m0.rs.
    let helper = built
        .pages
        .iter()
        .flat_map(|p| &p.members)
        .find(|s| s.name == "helper0")
        .expect("helper0 is a member");
    assert_eq!(
        (helper.location.file(), helper.location.line()),
        ("m0.rs", 5)
    );
    // `S0::run` calls `helper0`: an inferred Calls edge anchored at `run`.
    let calls = built
        .pages
        .iter()
        .flat_map(|p| &p.edges)
        .find(|e| e.relation() == "Calls" && e.target() == helper.id)
        .expect("run -> helper0");
    assert_eq!(calls.provenance(), Provenance::Inferred);
    assert_eq!(
        (calls.location().file(), calls.location().line()),
        ("m0.rs", 3)
    );

    let first = WikiSkeleton::new(built.pages).unwrap().to_json().unwrap();
    let again = community_pages(&store).expect("pages again");
    let second = WikiSkeleton::new(again.pages).unwrap().to_json().unwrap();
    assert_eq!(first, second);
}

/// An empty id identifies nothing (tests/community_process_id_audit.rs): a
/// `Community` with id `''` gets no page, and a symbol whose only membership
/// points at it is in no community, so it is neither listed nor counted as
/// crossing, and its edges are on no page.
#[test]
fn an_empty_community_id_makes_no_page_and_no_membership() {
    let (_dir, store) = fixture();
    insert(
        &store,
        "Community",
        vec![vec![
            ("id".into(), cypher_str("")),
            ("name".into(), cypher_str("ghost")),
        ]],
    );
    insert(
        &store,
        "Function",
        vec![node("src/d.rs::lost", "lost", Some(9))],
    );
    link(
        &store,
        "MemberOf_Function_Community",
        &[("src/d.rs::lost", "")],
    );
    link(
        &store,
        "Calls_Function_Function",
        &[
            ("src/d.rs::lost", "src/a.rs::f"),
            ("src/a.rs::g", "src/d.rs::lost"),
        ],
    );
    let built = community_pages(&store).expect("pages");
    let ids: Vec<&str> = built.pages.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, [C1, C2]);
    assert!(built
        .pages
        .iter()
        .all(|p| p.members.iter().all(|s| s.id != "src/d.rs::lost")));
    assert!(built
        .pages
        .iter()
        .all(|p| p.edges.iter().all(|e| e.source() != "src/d.rs::lost")));
    assert_eq!(
        page(&built, C1).cross_community_edges,
        BTreeMap::from([(C2.to_owned(), 2)]),
        "g -> lost does not cross: lost is in no community"
    );
    assert!(!built
        .unanchored_members
        .contains(&"src/d.rs::lost".to_owned()));
    WikiSkeleton::new(built.pages).expect("a valid skeleton");
}
