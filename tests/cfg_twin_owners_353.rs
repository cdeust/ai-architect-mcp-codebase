//! Issue #353, review round: an edge whose source or target is a twin type
//! resolves to NONE, unless the impl carries the gate of exactly one twin.
//!
//! Each case has a control fixture with the same shape and no twin, so a test
//! that passes because the edge is never produced at all cannot pass here.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::fs;

fn graph_of(source_text: &str) -> (tempfile::TempDir, GraphStore) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("lib.rs"), source_text).unwrap();
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&source, &graph).expect("index");
    let store = GraphStore::open_or_create(&graph).expect("open");
    resolver::resolve_graph(&store).expect("resolve");
    (tmp, store)
}

fn count(store: &GraphStore, cypher: &str) -> usize {
    store.execute_query(cypher).expect(cypher).rows.len()
}

const TWIN_STRUCTS: &str = "#[cfg(unix)]\nstruct S;\n#[cfg(not(unix))]\nstruct S;\n";
const HAS_METHOD: &str = "MATCH (a:Struct)-[r:HasMethod_Struct_Method]->(m:Method)";

#[test]
fn an_ungated_impl_for_twin_structs_has_no_owner_edge() {
    let (_t, store) = graph_of(&format!(
        "{TWIN_STRUCTS}impl S {{\n    fn m(&self) {{}}\n}}\n"
    ));
    assert_eq!(
        count(&store, "MATCH (s:Struct) WHERE s.name = 'S' RETURN s.id"),
        2
    );
    assert_eq!(
        count(&store, "MATCH (m:Method) WHERE m.name = 'm' RETURN m.id"),
        1
    );
    let owned = count(&store, &format!("{HAS_METHOD} RETURN m.id"));
    assert_eq!(owned, 0, "the impl was tied to a twin it does not name");
    let (_c, control) = graph_of("struct S;\nimpl S {\n    fn m(&self) {}\n}\n");
    assert_eq!(count(&control, &format!("{HAS_METHOD} RETURN m.id")), 1);
}

#[test]
fn an_impl_gated_like_one_twin_belongs_to_that_twin() {
    let (_t, store) = graph_of(&format!(
        "{TWIN_STRUCTS}#[cfg(unix)]\nimpl S {{\n    fn m(&self) {{}}\n}}\n"
    ));
    let owners = store
        .execute_query(&format!("{HAS_METHOD} RETURN a.id, m.id"))
        .unwrap()
        .rows;
    assert_eq!(owners.len(), 1, "{owners:?}");
    assert_eq!(owners[0][0], "lib.rs::S#cfg(unix)");
    assert_eq!(owners[0][1], "lib.rs::S#cfg(unix)::m");
}

#[test]
fn an_impl_whose_gate_names_no_twin_has_no_owner_edge() {
    let (_t, store) = graph_of(&format!(
        "{TWIN_STRUCTS}#[cfg(feature = \"x\")]\nimpl S {{\n    fn m(&self) {{}}\n}}\n"
    ));
    assert_eq!(count(&store, &format!("{HAS_METHOD} RETURN m.id")), 0);
}

#[test]
fn twin_enums_get_no_owner_edge_from_an_ungated_impl() {
    let (_t, store) = graph_of(
        "#[cfg(unix)]\nenum E { A }\n#[cfg(not(unix))]\nenum E { A }\nimpl E {\n    fn m(&self) {}\n}\n",
    );
    let owned = count(
        &store,
        "MATCH (a:Enum)-[r:HasMethod_Enum_Method]->(m:Method) RETURN m.id",
    );
    assert_eq!(owned, 0);
    assert_eq!(
        count(&store, "MATCH (e:Enum) WHERE e.name = 'E' RETURN e.id"),
        2
    );
}

const IMPL_TRAIT: &str = "struct T;\nimpl Base for T {\n    fn f(&self) {}\n}\n";
const IMPLEMENTS: &str = "MATCH (s:Struct)-[r:Implements_Struct_Trait]->(t:Trait) RETURN t.id";

#[test]
fn an_impl_of_a_twin_trait_gets_no_implements_edge() {
    let twin = format!(
        "#[cfg(unix)]\ntrait Base {{ fn f(&self); }}\n#[cfg(not(unix))]\ntrait Base {{ fn f(&self); }}\n{IMPL_TRAIT}"
    );
    let (_t, store) = graph_of(&twin);
    assert_eq!(count(&store, IMPLEMENTS), 0, "a twin trait was chosen");
    let control = format!("trait Base {{ fn f(&self); }}\n{IMPL_TRAIT}");
    let (_c, control_store) = graph_of(&control);
    assert_eq!(count(&control_store, IMPLEMENTS), 1);
}

#[test]
fn a_derive_naming_a_twin_trait_gets_no_implements_edge() {
    let twin = "#[cfg(unix)]\ntrait Mark {}\n#[cfg(not(unix))]\ntrait Mark {}\n#[derive(Mark)]\nstruct T;\n";
    let (_t, store) = graph_of(twin);
    assert_eq!(count(&store, IMPLEMENTS), 0);
    let (_c, control) = graph_of("trait Mark {}\n#[derive(Mark)]\nstruct T;\n");
    assert_eq!(count(&control, IMPLEMENTS), 1);
}

#[test]
fn a_derive_on_a_twin_struct_names_the_twin_and_still_resolves_a_plain_trait() {
    let twin =
        "trait Mark {}\n#[cfg(unix)]\n#[derive(Mark)]\nstruct T;\n#[cfg(not(unix))]\nstruct T;\n";
    let (_t, store) = graph_of(twin);
    let rows = store
        .execute_query("MATCH (s:Struct)-[r:Implements_Struct_Trait]->(t:Trait) RETURN s.id")
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0][0], "lib.rs::T#cfg(unix)");
}

#[test]
fn a_field_typed_by_a_twin_struct_gets_no_uses_edge() {
    let twin = format!("{TWIN_STRUCTS}struct Holder {{\n    s: S,\n}}\n");
    let (_t, store) = graph_of(&twin);
    let uses = "MATCH (f:Field)-[r:Uses_Field_Struct]->(s:Struct) RETURN s.id";
    assert_eq!(count(&store, uses), 0, "a twin struct was chosen");
    let (_c, control) = graph_of("struct S;\nstruct Holder {\n    s: S,\n}\n");
    assert_eq!(count(&control, uses), 1);
}
