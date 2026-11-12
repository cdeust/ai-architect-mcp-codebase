use super::*;
use crate::graph_store::{cypher_str, PropEdge};
use crate::wiki::{attribute_missed, WikiSkeleton};

const C1: &str = "community::louvain::1::0";
const C2: &str = "community::louvain::1::1";

fn open_store() -> (tempfile::TempDir, GraphStore) {
    let dir = tempfile::Builder::new()
        .prefix("wiki_god_nodes_")
        .tempdir()
        .expect("tempdir");
    let store = GraphStore::open_or_create(&dir.path().join("graph")).expect("open");
    store.create_schema().expect("schema");
    (dir, store)
}

fn function(id: &str, line: u32) -> Vec<(String, String)> {
    let name = id.rsplit("::").next().unwrap_or(id);
    vec![
        ("id".to_owned(), cypher_str(id)),
        ("name".to_owned(), cypher_str(name)),
        ("qualified_name".to_owned(), cypher_str(id)),
        ("start_line".to_owned(), line.to_string()),
        ("end_line".to_owned(), line.to_string()),
    ]
}

fn link(store: &GraphStore, rel: &str, pairs: &[(&str, &str)]) {
    let edges: Vec<PropEdge> = pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string(), Vec::new()))
        .collect();
    store.bulk_insert_edges(rel, &edges).expect(rel);
}

fn communities(store: &GraphStore) {
    let rows = [C1, C2]
        .iter()
        .enumerate()
        .map(|(i, id)| {
            vec![
                ("id".to_owned(), cypher_str(id)),
                ("name".to_owned(), cypher_str(&format!("community_{i}"))),
            ]
        })
        .collect::<Vec<_>>();
    store
        .bulk_insert_nodes("Community", &rows)
        .expect("Community");
}

/// A star: `hub` (c1) calls l1..l5 (c1) and l6 (c2); `weird::w` (c2, no
/// file extension, so no `file:line`) calls `hub`. Degrees: hub 7, every
/// other symbol 1. `src/c.rs::orphan` is in no community and calls `hub`:
/// it is no clustered symbol, so neither its degree nor its edges count,
/// in either direction.
/// An `OBSERVED_CALLS_*` edge has no provenance and does not count either.
fn star() -> (tempfile::TempDir, GraphStore) {
    let (dir, store) = open_store();
    communities(&store);
    let mut rows = vec![function("src/a.rs::hub", 10)];
    for i in 1..=5 {
        rows.push(function(&format!("src/a.rs::l{i}"), 20 + i));
    }
    rows.push(function("src/b.rs::l6", 3));
    rows.push(function("weird::w", 5));
    rows.push(function("src/c.rs::orphan", 1));
    store
        .bulk_insert_nodes("Function", &rows)
        .expect("Function");
    let mut member: Vec<(String, &str)> = vec![("src/a.rs::hub".into(), C1)];
    for i in 1..=5 {
        member.push((format!("src/a.rs::l{i}"), C1));
    }
    member.push(("src/b.rs::l6".into(), C2));
    member.push(("weird::w".into(), C2));
    let pairs: Vec<(&str, &str)> = member.iter().map(|(a, b)| (a.as_str(), *b)).collect();
    link(&store, "MemberOf_Function_Community", &pairs);
    let leaves: Vec<String> = (1..=5)
        .map(|i| format!("src/a.rs::l{i}"))
        .chain(["src/b.rs::l6".to_owned()])
        .collect();
    let mut calls: Vec<(&str, &str)> = leaves
        .iter()
        .map(|l| ("src/a.rs::hub", l.as_str()))
        .collect();
    calls.push(("weird::w", "src/a.rs::hub"));
    calls.push(("src/c.rs::orphan", "src/a.rs::hub"));
    // An edge into the unclustered orphan counts toward nobody's degree.
    calls.push(("src/a.rs::l1", "src/c.rs::orphan"));
    link(&store, "Calls_Function_Function", &calls);
    link(
        &store,
        "OBSERVED_CALLS_Function_Function",
        &[("src/a.rs::l1", "src/a.rs::l2")],
    );
    (dir, store)
}

#[test]
fn type7_quartiles_match_the_reference_values() {
    // R: quantile(1:8, c(.25, .75)) = 2.75, 6.25 (type 7, the default).
    let fence = Fence::of(&[8, 3, 1, 7, 2, 6, 5, 4]).expect("non-empty");
    assert_eq!((fence.q1_quarters(), fence.q3_quarters()), (11, 25));
    // 4 · (6.25 + 3 · 3.5) = 67: 16 (64) is inside, 17 (68) beyond.
    assert_eq!(fence.limit_quarters(), 67);
    assert!(!fence.exceeds(16));
    assert!(fence.exceeds(17));
    // R: quantile(c(0, 10), c(.25, .75)) = 2.5, 7.5.
    let two = Fence::of(&[10, 0]).unwrap();
    assert_eq!((two.q1_quarters(), two.q3_quarters()), (10, 30));
    // R: quantile(c(1, 2, 3, 4, 100), .75) = 4; n − 1 = 4, so h is whole.
    let five = Fence::of(&[1, 2, 3, 4, 100]).unwrap();
    assert_eq!((five.q1_quarters(), five.q3_quarters()), (8, 16));
}

#[test]
fn a_degree_equal_to_the_fence_is_not_beyond_it() {
    // IQR = 0: the fence is Q3 itself.
    let flat = Fence::of(&[2, 2, 2, 2, 9]).unwrap();
    assert_eq!(flat.limit_quarters(), 8);
    assert!(!flat.exceeds(2));
    assert!(flat.exceeds(3));
    // One symbol: Q1 = Q3 = its degree, which is never beyond itself.
    let one = Fence::of(&[5]).unwrap();
    assert!(!one.exceeds(5));
    assert!(Fence::of(&[]).is_none());
}

#[test]
fn the_hub_of_a_star_is_the_only_god_node() {
    let (_dir, store) = star();
    let built = god_node_pages(&store).expect("pages");
    let fence = built.fence.expect("clustered symbols");
    assert_eq!((fence.q1_quarters(), fence.q3_quarters()), (4, 4));
    assert_eq!(built.pages.len(), 1, "{:?}", built.pages);
    let page = &built.pages[0];
    assert_eq!(page.kind, PageKind::GodNode);
    assert_eq!(
        (page.id.as_str(), page.title.as_str()),
        ("src/a.rs::hub", "hub")
    );
    let members: Vec<(&str, &str, &str, u32)> = page
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
    assert_eq!(members, [("src/a.rs::hub", "Function", "src/a.rs", 10)]);
    // Six outgoing calls, anchored at the hub; the incoming one from
    // `weird::w` has no anchor and the orphan's is not a clustered edge.
    let mut edges: Vec<(&str, &str, &str, Provenance, u32)> = page
        .edges
        .iter()
        .map(|e| {
            (
                e.source(),
                e.target(),
                e.relation(),
                e.provenance(),
                e.location().line(),
            )
        })
        .collect();
    edges.sort();
    let expected: Vec<String> = (1..=5)
        .map(|i| format!("src/a.rs::l{i}"))
        .chain(["src/b.rs::l6".to_owned()])
        .collect();
    let mut want: Vec<(&str, &str, &str, Provenance, u32)> = expected
        .iter()
        .map(|t| {
            (
                "src/a.rs::hub",
                t.as_str(),
                "Calls",
                Provenance::Inferred,
                10,
            )
        })
        .collect();
    want.sort();
    assert_eq!(edges, want);
    assert_eq!(built.unanchored_edges, 1);
    assert!(built.unanchored_members.is_empty());
    // l6 and weird::w are in c2: two crossing edges, anchored or not.
    assert_eq!(
        page.cross_community_edges.iter().collect::<Vec<_>>(),
        [(&C2.to_owned(), &2)]
    );
}

#[test]
fn a_graph_with_the_same_degree_everywhere_has_no_god_node() {
    let (_dir, store) = open_store();
    communities(&store);
    let rows: Vec<_> = ["a", "b", "c"]
        .iter()
        .enumerate()
        .map(|(i, n)| function(&format!("src/a.rs::{n}"), i as u32 + 1))
        .collect();
    store.bulk_insert_nodes("Function", &rows).unwrap();
    link(
        &store,
        "MemberOf_Function_Community",
        &[
            ("src/a.rs::a", C1),
            ("src/a.rs::b", C1),
            ("src/a.rs::c", C2),
        ],
    );
    link(
        &store,
        "Calls_Function_Function",
        &[
            ("src/a.rs::a", "src/a.rs::b"),
            ("src/a.rs::b", "src/a.rs::c"),
            ("src/a.rs::c", "src/a.rs::a"),
        ],
    );
    let built = god_node_pages(&store).expect("pages");
    assert_eq!(built.fence.map(|f| f.limit_quarters()), Some(8));
    assert!(built.pages.is_empty(), "{:?}", built.pages);
}

#[test]
fn a_self_edge_counts_once_toward_the_degree() {
    let (_dir, store) = open_store();
    communities(&store);
    let names = ["a", "b", "c", "d", "e"];
    let rows: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, n)| function(&format!("src/a.rs::{n}"), i as u32 + 1))
        .collect();
    store.bulk_insert_nodes("Function", &rows).unwrap();
    let ids: Vec<String> = names.iter().map(|n| format!("src/a.rs::{n}")).collect();
    let member: Vec<(&str, &str)> = ids.iter().map(|i| (i.as_str(), C1)).collect();
    link(&store, "MemberOf_Function_Community", &member);
    // A cycle a -> b -> c -> d -> a, then e recurses and calls a.
    // Degrees: a 3, b c d 2, e 2 (its self-edge once). Sorted
    // [2, 2, 2, 2, 3]: Q1 = Q3 = 2, fence 2. Only a is beyond; e would be
    // too if its self-edge counted twice.
    link(
        &store,
        "Calls_Function_Function",
        &[
            ("src/a.rs::a", "src/a.rs::b"),
            ("src/a.rs::b", "src/a.rs::c"),
            ("src/a.rs::c", "src/a.rs::d"),
            ("src/a.rs::d", "src/a.rs::a"),
            ("src/a.rs::e", "src/a.rs::e"),
            ("src/a.rs::e", "src/a.rs::a"),
        ],
    );
    let built = god_node_pages(&store).expect("pages");
    let fence = built.fence.unwrap();
    assert_eq!((fence.q1_quarters(), fence.q3_quarters()), (8, 8));
    let ids: Vec<&str> = built.pages.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["src/a.rs::a"]);
}

#[test]
fn an_unanchored_god_node_is_returned_not_dropped() {
    let (_dir, store) = open_store();
    communities(&store);
    let mut rows = vec![function("weird::big", 1)];
    for i in 1..=4 {
        rows.push(function(&format!("src/a.rs::l{i}"), i));
    }
    store.bulk_insert_nodes("Function", &rows).unwrap();
    let ids: Vec<String> = std::iter::once("weird::big".to_owned())
        .chain((1..=4).map(|i| format!("src/a.rs::l{i}")))
        .collect();
    let member: Vec<(&str, &str)> = ids.iter().map(|i| (i.as_str(), C1)).collect();
    link(&store, "MemberOf_Function_Community", &member);
    let calls: Vec<(&str, &str)> = ids[1..]
        .iter()
        .map(|l| ("weird::big", l.as_str()))
        .collect();
    link(&store, "Calls_Function_Function", &calls);
    let built = god_node_pages(&store).expect("pages");
    assert!(built.pages.is_empty());
    assert_eq!(built.unanchored_members, ["weird::big"]);
    assert_eq!(built.unanchored_edges, 0);
}

#[test]
fn a_graph_with_no_clustered_symbol_has_no_fence() {
    let (_dir, store) = open_store();
    let built = god_node_pages(&store).expect("pages");
    assert_eq!(built.fence, None);
    assert!(built.pages.is_empty());
}

/// Criterion 6 on this path, and the pages pass the skeleton's checks; a
/// missed call site in the hub's body lands on the god-node page (criterion
/// 5), since attribution matches pages by member.
#[test]
fn god_node_pages_form_a_deterministic_skeleton_with_coverage() {
    let (_dir, store) = star();
    let run = || {
        let mut pages = god_node_pages(&store).expect("pages").pages;
        attribute_missed(&mut pages, &store).expect("missed");
        WikiSkeleton::new(pages).unwrap().to_json().unwrap()
    };
    let first = run();
    assert_eq!(first, run());
    assert!(first.contains(r#""kind": "god_node""#), "{first}");
    assert!(first.contains(r#""coverage": []"#), "{first}");
}
