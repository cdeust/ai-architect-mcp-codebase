//! Issue #353, review round: twin ids depend on the item's whole gate (its own
//! `#[cfg]` plus those of every enclosing `mod`, `impl`, `trait` and `fn`), so an
//! edit of a gate can rename ids. These tests edit a gate, refresh the graph
//! incrementally with an unchanged file that calls into the edited one, and pin
//! what stays valid.
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
        let fixture = Fixture {
            _tmp: tmp,
            source,
            graph,
            manifest,
        };
        fixture.resolve();
        fixture
    }

    fn store(&self) -> GraphStore {
        GraphStore::open_or_create(&self.graph).expect("open")
    }

    fn resolve(&self) {
        resolver::resolve_graph(&self.store()).expect("resolve");
    }

    fn edit(&self, name: &str, text: &str) {
        fs::write(self.source.join(name), text).unwrap();
    }

    fn refresh(&self) {
        let prior = indexer::manifest::load(&self.manifest).expect("manifest loads");
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

fn edges_to(fixture: &Fixture, name: &str) -> Vec<Vec<String>> {
    fixture.rows(&format!(
        "MATCH (c:CallSite)-[r:Calls_CallSite_Function]->(f:Function) WHERE f.name = '{name}' RETURN f.id, r.confidence"
    ))
}

fn ids_of(fixture: &Fixture, label: &str, name: &str) -> Vec<String> {
    let mut ids: Vec<String> = fixture
        .rows(&format!(
            "MATCH (n:{label}) WHERE n.name = '{name}' RETURN n.id"
        ))
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    ids.sort();
    ids
}

const USER: &str = "pub fn caller() {\n    pick();\n}\n";
const ONE: &str = "#[cfg(unix)]\npub fn pick() {}\n";
const TWO: &str = "#[cfg(unix)]\npub fn pick() {}\n#[cfg(not(unix))]\npub fn pick() {}\n";

/// A `pick` that was one node with a 0.95 edge becomes a twin pair when the twin
/// is added to `lib.rs` and only `lib.rs` is refreshed: the edge of the
/// unchanged `user.rs` must not survive onto either twin, and after the next
/// resolve the site is open with the reason `cfg_twins`.
#[test]
fn a_function_that_becomes_a_twin_loses_its_incoming_edge_and_the_site_says_why() {
    let f = Fixture::new(&[("lib.rs", ONE), ("user.rs", USER)]);
    let before = edges_to(&f, "pick");
    assert_eq!(before.len(), 1, "control: one edge before the edit");
    assert_eq!(before[0][0], "lib.rs::pick");

    f.edit("lib.rs", TWO);
    f.refresh();
    assert_eq!(
        ids_of(&f, "Function", "pick"),
        ["lib.rs::pick#cfg(not(unix))", "lib.rs::pick#cfg(unix)"]
    );
    assert!(
        edges_to(&f, "pick").is_empty(),
        "an edge survived onto a twin"
    );

    // Incremental refresh is the index stage: it does not run the resolver, and the
    // flag of an unchanged file's site is left as it was for ANY purged target (a
    // deleted function does the same). The next resolve re-derives it.
    f.resolve();
    assert!(edges_to(&f, "pick").is_empty());
    let site = f.rows(
        "MATCH (c:CallSite) WHERE c.callee_name = 'pick' RETURN c.is_resolved, c.unresolved_reason",
    );
    assert_eq!(site.len(), 1);
    assert_eq!(site[0][0].to_lowercase(), "false", "{site:?}");
    assert_eq!(site[0][1], "cfg_twins", "{site:?}");
}

/// The reverse: the twin is deleted, the survivor returns to its plain id and the
/// next resolve links the caller again.
#[test]
fn a_twin_that_is_removed_returns_to_its_plain_id_and_is_linked_again() {
    let f = Fixture::new(&[("lib.rs", TWO), ("user.rs", USER)]);
    assert!(
        edges_to(&f, "pick").is_empty(),
        "control: twins get no edge"
    );
    f.edit("lib.rs", ONE);
    f.refresh();
    assert_eq!(ids_of(&f, "Function", "pick"), ["lib.rs::pick"]);
    f.resolve();
    let after = edges_to(&f, "pick");
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0][0], "lib.rs::pick");
}

/// Moving a gate from a `mod` to its item does not rename an item that has no
/// twin: its id stays plain, so the edge of the unchanged caller is relinked.
#[test]
fn moving_a_gate_from_a_module_to_its_item_keeps_the_id_of_an_item_without_a_twin() {
    let gated_mod = "#[cfg(unix)]\npub mod m {\n    pub fn pick() {}\n}\n";
    let gated_fn = "pub mod m {\n    #[cfg(unix)]\n    pub fn pick() {}\n}\n";
    let f = Fixture::new(&[("lib.rs", gated_mod), ("user.rs", USER)]);
    let before = edges_to(&f, "pick");
    assert_eq!(before.len(), 1, "{before:?}");
    f.edit("lib.rs", gated_fn);
    f.refresh();
    assert_eq!(ids_of(&f, "Function", "pick"), ["lib.rs::m::pick"]);
    let after = edges_to(&f, "pick");
    assert_eq!(after, before, "the relinked edge differs");
}

/// Moving the gate of a twin pair from the items to their `mod` renames the twin
/// ids: the old ones are gone (no node keeps a stale id), the new ones exist, and
/// no call edge points at either.
#[test]
fn moving_the_gates_of_twins_from_items_to_modules_renames_them_cleanly() {
    let on_items = "pub mod m {\n    #[cfg(unix)]\n    pub fn pick() {}\n    #[cfg(not(unix))]\n    pub fn pick() {}\n}\n";
    let on_mods = "#[cfg(unix)]\npub mod m {\n    pub fn pick() {}\n}\n#[cfg(not(unix))]\npub mod m {\n    pub fn pick() {}\n}\n";
    let f = Fixture::new(&[("lib.rs", on_items), ("user.rs", USER)]);
    assert_eq!(
        ids_of(&f, "Function", "pick"),
        [
            "lib.rs::m::pick#cfg(not(unix))",
            "lib.rs::m::pick#cfg(unix)"
        ]
    );
    f.edit("lib.rs", on_mods);
    f.refresh();
    assert_eq!(
        ids_of(&f, "Function", "pick"),
        [
            "lib.rs::m#cfg(not(unix))::pick",
            "lib.rs::m#cfg(unix)::pick"
        ]
    );
    assert!(edges_to(&f, "pick").is_empty());
}
