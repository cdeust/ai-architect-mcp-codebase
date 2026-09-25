//! Issue #353, second review round: what the resolvers do when only ONE twin
//! survives an incremental edit that removed its sibling, and which receiver id
//! the parser stamps for `impl Trait for Type`.
//!
//! The survivor is reparsed with its file, so it has a plain id and no gate again
//! (a group of one is not a twin). An edge to it is acceptable only with that
//! current id; an edge naming the old `#cfg(..)` id would be stale.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::fs;
use std::path::PathBuf;

struct Fixture {
    _tmp: tempfile::TempDir,
    source: PathBuf,
    graph: PathBuf,
    manifest: PathBuf,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        for (name, text) in files {
            fs::write(source.join(name), text).unwrap();
        }
        let graph = tmp.path().join("graph");
        let manifest = tmp.path().join("manifest.json");
        indexer::index_codebase(&source, &graph).expect("index");
        indexer::write_full_manifest(&source, &manifest, &indexer::IndexOptions::default())
            .expect("manifest");
        let f = Fixture {
            _tmp: tmp,
            source,
            graph,
            manifest,
        };
        f.resolve();
        f
    }

    fn store(&self) -> GraphStore {
        GraphStore::open_or_create(&self.graph).expect("open")
    }

    fn resolve(&self) {
        resolver::resolve_graph(&self.store()).expect("resolve");
    }

    fn edit(&self, name: &str, text: &str) {
        fs::write(self.source.join(name), text).unwrap();
        let prior = indexer::manifest::load(&self.manifest).expect("manifest");
        indexer::index_incremental(
            &self.source,
            &self.graph,
            &self.manifest,
            &indexer::IndexOptions::default(),
            &prior,
        )
        .expect("incremental refresh");
    }

    fn rows(&self, cypher: &str) -> Vec<Vec<String>> {
        self.store().execute_query(cypher).expect(cypher).rows
    }
}

const TWIN_STRUCTS: &str = "#[cfg(unix)]\npub struct S;\n#[cfg(not(unix))]\npub struct S;\n";
const ONE_STRUCT: &str = "#[cfg(unix)]\npub struct S;\n";
const HOLDER: &str = "struct Holder {\n    s: S,\n}\n";
const USES: &str = "MATCH (f:Field)-[r:Uses_Field_Struct]->(s:Struct) RETURN s.id";

/// `uses.rs` `find_type_target` with `types.len() == 1`: the twin pair had no
/// edge; after the sibling is removed the survivor is the one type of that name,
/// the edge is made, and it names the survivor's CURRENT id, not the old suffix.
#[test]
fn a_field_typed_by_the_surviving_twin_uses_its_current_plain_id() {
    let f = Fixture::new(&[("lib.rs", TWIN_STRUCTS), ("user.rs", HOLDER)]);
    assert!(f.rows(USES).is_empty(), "control: twins get no edge");
    f.edit("lib.rs", ONE_STRUCT);
    f.resolve();
    assert_eq!(f.rows(USES), [["lib.rs::S"]]);
    let node = f.rows("MATCH (s:Struct) WHERE s.name = 'S' RETURN s.id, s.cfg_gate");
    assert_eq!(
        node,
        [["lib.rs::S", ""]],
        "the survivor kept a stale id or gate"
    );
}

/// `resolve_impl_trait_blocks` after the same edit: the impl's methods carry the
/// receiver qn the reparse stamped, which is the survivor's current id.
#[test]
fn an_impl_trait_block_on_the_surviving_twin_links_to_its_current_id() {
    let trait_and_impl =
        "trait Base {\n    fn f(&self);\n}\nimpl Base for S {\n    fn f(&self) {}\n}\n";
    let twin = format!("{TWIN_STRUCTS}{trait_and_impl}");
    let f = Fixture::new(&[("lib.rs", &twin)]);
    let implements = "MATCH (s:Struct)-[r:Implements_Struct_Trait]->(t:Trait) RETURN s.id, t.id";
    assert!(
        f.rows(implements).is_empty(),
        "control: twin receiver, no edge"
    );
    f.edit("lib.rs", &format!("{ONE_STRUCT}{trait_and_impl}"));
    f.resolve();
    assert_eq!(f.rows(implements), [["lib.rs::S", "lib.rs::Base"]]);
}

/// The stamping `resolve_impl_trait_blocks` reads: for a type without twins the
/// method's `receiver_type` is the plain qn of the struct, which is its node id;
/// for an impl gated exactly like one twin it is that twin's id.
#[test]
fn the_parser_stamps_the_receiver_qn_the_resolver_looks_up() {
    let plain =
        "trait Base {\n    fn f(&self);\n}\nstruct S;\nimpl Base for S {\n    fn f(&self) {}\n}\n";
    let f = Fixture::new(&[("lib.rs", plain)]);
    assert_eq!(
        f.rows("MATCH (m:Method) WHERE m.trait_name = 'Base' RETURN m.receiver_type"),
        [["lib.rs::S"]]
    );
    let owned = format!(
        "trait Base {{\n    fn f(&self);\n}}\n{TWIN_STRUCTS}#[cfg(unix)]\nimpl Base for S {{\n    fn f(&self) {{}}\n}}\n"
    );
    let g = Fixture::new(&[("lib.rs", &owned)]);
    assert_eq!(
        g.rows("MATCH (m:Method) WHERE m.trait_name = 'Base' RETURN m.receiver_type"),
        [["lib.rs::S#cfg(unix)"]]
    );
    assert_eq!(
        g.rows("MATCH (s:Struct)-[r:Implements_Struct_Trait]->(t:Trait) RETURN s.id, t.id"),
        [["lib.rs::S#cfg(unix)", "lib.rs::Base"]]
    );
}
