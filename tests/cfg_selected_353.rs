//! Issue #353, part B: a call to twins of one item is resolved to the twin the
//! build compiles when the build decides it, and left open otherwise.
//!
//! Every fixture is a real Cargo package, so `cargo metadata` gives the package's
//! default features and the indexer writes `cfg_active` from them. The edge is
//! `cfg-selected`, at most 0.9, and only to a twin the default features (or the
//! caller's own gate) show compiled.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::fs;
use std::path::PathBuf;

const PICK: &str = "#[cfg(feature = \"fast\")]\npub fn pick() -> u32 {\n    1\n}\n\n\
#[cfg(not(feature = \"fast\"))]\npub fn pick() -> u32 {\n    2\n}\n\n\
pub fn caller() -> u32 {\n    pick()\n}\n";

fn manifest(features: &str) -> String {
    format!("[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\n{features}\n")
}

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    graph: PathBuf,
}

impl Project {
    fn new(features: &str, files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("crate");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("Cargo.toml"), manifest(features)).unwrap();
        for (name, text) in files {
            fs::write(root.join(name), text).unwrap();
        }
        let graph = tmp.path().join("graph");
        indexer::index_codebase(&root, &graph).expect("index");
        let project = Project {
            _tmp: tmp,
            root,
            graph,
        };
        project.resolve();
        project
    }

    fn store(&self) -> GraphStore {
        GraphStore::open_or_create(&self.graph).expect("open graph")
    }

    fn resolve(&self) {
        resolver::resolve_graph(&self.store()).expect("resolve");
    }

    fn rows(&self, cypher: &str) -> Vec<Vec<String>> {
        self.store().execute_query(cypher).expect(cypher).rows
    }

    /// `(caller, target, method, confidence)` of every call edge into `name`.
    fn calls_into(&self, name: &str) -> Vec<Vec<String>> {
        let mut edges = self.rows(&format!(
            "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) WHERE b.name = '{name}' \
             RETURN a.name, b.qualified_name, r.resolution_method, r.confidence"
        ));
        edges.sort();
        edges
    }

    fn activity(&self, label: &str, name: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .rows(&format!(
                "MATCH (n:{label}) WHERE n.name = '{name}' RETURN n.cfg_gate, n.cfg_active"
            ))
            .into_iter()
            .map(|r| (r[0].clone(), r[1].clone()))
            .collect();
        out.sort();
        out
    }
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

/// The issue's reproduction: `fast` is off by default, so the build compiles the
/// negated twin, and the edge goes there with the label that says how it was got.
#[test]
fn the_call_reaches_the_twin_the_default_features_compile() {
    let p = Project::new("fast = []", &[("src/lib.rs", PICK)]);
    assert_eq!(
        p.activity("Function", "pick"),
        [
            pair("feature=fast", "inactive"),
            pair("not(feature=fast)", "active")
        ]
    );
    let edges = p.calls_into("pick");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert_eq!(edges[0][0], "caller");
    assert!(
        edges[0][1].ends_with("#cfg(not(feature=fast))"),
        "{edges:?}"
    );
    assert_eq!(edges[0][2], "cfg-selected");
    assert_eq!(edges[0][3], "0.85");
    let site = p.rows(
        "MATCH (c:CallSite) WHERE c.callee_name = 'pick' RETURN c.is_resolved, c.unresolved_reason",
    );
    assert_eq!(site.len(), 1);
    assert_eq!(site[0][0].to_lowercase(), "true", "{site:?}");
    assert_eq!(
        site[0][1], "",
        "a resolved site carries no reason: {site:?}"
    );
}

/// The same crate with `fast` in the default features compiles the other twin:
/// the choice follows the build, not the order of the source.
#[test]
fn a_default_feature_moves_the_edge_to_the_other_twin() {
    let p = Project::new("default = [\"fast\"]\nfast = []", &[("src/lib.rs", PICK)]);
    let edges = p.calls_into("pick");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(edges[0][1].ends_with("#cfg(feature=fast)"), "{edges:?}");
}

/// A feature turned on through another default feature counts as on.
#[test]
fn a_feature_enabled_through_another_default_feature_is_on() {
    let p = Project::new(
        "default = [\"turbo\"]\nturbo = [\"fast\"]\nfast = []",
        &[("src/lib.rs", PICK)],
    );
    let edges = p.calls_into("pick");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(edges[0][1].ends_with("#cfg(feature=fast)"), "{edges:?}");
}

/// `kani` is a bare option: the default build does not decide it, so neither
/// twin is shown compiled and the call stays open with the reason.
#[test]
fn twins_the_default_profile_does_not_decide_stay_open() {
    let source = "#[cfg(kani)]\npub fn pick() -> u32 {\n    1\n}\n\n\
#[cfg(not(kani))]\npub fn pick() -> u32 {\n    2\n}\n\n\
pub fn caller() -> u32 {\n    pick()\n}\n";
    let p = Project::new("", &[("src/lib.rs", source)]);
    assert_eq!(
        p.activity("Function", "pick"),
        [pair("kani", "unknown"), pair("not(kani)", "unknown")]
    );
    assert!(p.calls_into("pick").is_empty());
    let site = p.rows(
        "MATCH (c:CallSite) WHERE c.callee_name = 'pick' RETURN c.is_resolved, c.unresolved_reason",
    );
    assert_eq!(site[0][0].to_lowercase(), "false", "{site:?}");
    assert_eq!(site[0][1], "cfg_twins", "{site:?}");
}

/// Callers that are themselves twins under `kani` and its negation each reach
/// the twin of their own gate, without the default profile deciding anything:
/// the gate in the caller's id is the evidence. A caller whose id carries no gate
/// (an item under `#[cfg(kani)]` that has no twin) decides nothing and stays open.
#[test]
fn callers_that_are_twins_reach_the_twin_of_their_own_gate() {
    let source = "#[cfg(kani)]\npub fn pick() -> u32 {\n    1\n}\n\n\
#[cfg(not(kani))]\npub fn pick() -> u32 {\n    2\n}\n\n\
#[cfg(kani)]\npub fn run() -> u32 {\n    pick()\n}\n\n\
#[cfg(not(kani))]\npub fn run() -> u32 {\n    pick()\n}\n\n\
#[cfg(kani)]\npub fn proof() -> u32 {\n    pick()\n}\n";
    let p = Project::new("", &[("src/lib.rs", source)]);
    let edges = p.calls_into("pick");
    assert_eq!(edges.len(), 2, "{edges:?}");
    let pairs: Vec<(&str, &str)> = edges
        .iter()
        .map(|e| (e[0].as_str(), e[1].as_str()))
        .collect();
    assert!(
        pairs.iter().any(|(_, t)| t.ends_with("pick#cfg(kani)")),
        "{edges:?}"
    );
    assert!(
        pairs
            .iter()
            .any(|(_, t)| t.ends_with("pick#cfg(not(kani))")),
        "{edges:?}"
    );
    let open = p.rows(
        "MATCH (c:CallSite) WHERE c.callee_name = 'pick' AND c.is_resolved = false \
         RETURN c.unresolved_reason",
    );
    assert_eq!(
        open,
        [["cfg_twins"]],
        "the `proof` call decides nothing: {open:?}"
    );
}

/// Twins in a module the default features compile out are all inactive.
#[test]
fn twins_in_a_compiled_out_module_are_inactive() {
    let files = [
        ("src/lib.rs", "#[cfg(feature = \"extra\")]\nmod extra;\n"),
        (
            "src/extra.rs",
            "#[cfg(unix)]\npub fn a() {}\n#[cfg(not(unix))]\npub fn a() {}\n",
        ),
    ];
    let p = Project::new("extra = []", &files);
    assert_eq!(
        p.activity("Function", "a"),
        [pair("not(unix)", "inactive"), pair("unix", "inactive")]
    );
}

/// The two twins of a method, in one `impl`: the receiver call reaches the one
/// the build compiles, never the other.
#[test]
fn a_receiver_call_reaches_the_twin_method_the_build_compiles() {
    let source = "pub struct S;\nimpl S {\n\
    #[cfg(feature = \"fast\")]\n    pub fn m(&self) -> u32 {\n        1\n    }\n\
    #[cfg(not(feature = \"fast\"))]\n    pub fn m(&self) -> u32 {\n        2\n    }\n}\n\
pub fn user(s: &S) -> u32 {\n    s.m()\n}\n";
    let p = Project::new("fast = []", &[("src/lib.rs", source)]);
    let edges = p.rows(
        "MATCH (a:Function)-[r:Calls_Function_Method]->(b:Method) WHERE b.name = 'm' \
         RETURN a.name, b.qualified_name, r.resolution_method, r.confidence",
    );
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(
        edges[0][1].ends_with("#cfg(not(feature=fast))"),
        "the call did not reach the twin the build compiles: {edges:?}"
    );
    assert_eq!(edges[0][2], "cfg-selected", "{edges:?}");
    assert!(
        p.rows("MATCH (n:Method) WHERE n.name = 'm' RETURN n.id")
            .len()
            == 2,
        "both twin methods are nodes"
    );
}

/// The features are recomputed when only `Cargo.toml` changes: no file is
/// reparsed, yet `cfg_active` and, after a resolve, the edge follow the new
/// default features.
#[test]
fn an_incremental_refresh_recomputes_activity_when_the_features_change() {
    let p = Project::new("fast = []", &[("src/lib.rs", PICK)]);
    let manifest_path = p.graph.parent().unwrap().join("manifest.json");
    indexer::write_full_manifest(&p.root, &manifest_path, &indexer::IndexOptions::default())
        .expect("manifest");
    fs::write(
        p.root.join("Cargo.toml"),
        manifest("default = [\"fast\"]\nfast = []"),
    )
    .unwrap();
    let prior = indexer::manifest::load(&manifest_path).expect("manifest loads");
    indexer::index_incremental(
        &p.root,
        &p.graph,
        &manifest_path,
        &indexer::IndexOptions::default(),
        &prior,
    )
    .expect("incremental refresh");
    assert_eq!(
        p.activity("Function", "pick"),
        [
            pair("feature=fast", "active"),
            pair("not(feature=fast)", "inactive")
        ]
    );
    p.resolve();
    let edges = p.calls_into("pick");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(edges[0][1].ends_with("#cfg(feature=fast)"), "{edges:?}");
}

/// Without a Cargo map nothing decides the ungated call, so it stays open: the
/// absence of the map is never read as "the feature is off".
#[test]
fn without_a_cargo_manifest_the_ungated_call_stays_open() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("lib.rs"), PICK).unwrap();
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&source, &graph).expect("index");
    let store = GraphStore::open_or_create(&graph).unwrap();
    resolver::resolve_graph(&store).expect("resolve");
    let twins = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'pick' RETURN f.cfg_active")
        .unwrap()
        .rows;
    assert!(twins.iter().all(|r| r[0] == "unknown"), "{twins:?}");
    let edges = store
        .execute_query("MATCH ()-[r:Calls_Function_Function]->(f:Function) WHERE f.name = 'pick' RETURN r.confidence")
        .unwrap()
        .rows;
    assert!(edges.is_empty(), "{edges:?}");
}

/// A graph written by the first part of this work has `cfg_gate` but no
/// `cfg_active`: every reader treats the twins as `unknown` and fails closed (no
/// edge, the reason `cfg_twins`) instead of erroring or choosing, and the next
/// index pass adds the column and decides them.
#[test]
fn a_graph_without_the_activity_column_fails_closed_and_is_repaired_by_an_index_pass() {
    let p = Project::new("fast = []", &[("src/lib.rs", PICK)]);
    let store = p.store();
    for label in ai_architect_mcp::graph_store::CFG_GATE_LABELS {
        store
            .execute_query(&format!("ALTER TABLE {label} DROP cfg_active"))
            .expect("drop the column to imitate the graph of the first part");
    }
    let summary = store.cfg_twin_summary();
    assert_eq!(
        (summary.members, summary.unknown, summary.active),
        (2, 2, 0)
    );
    drop(store);
    p.resolve();
    assert!(
        p.calls_into("pick").is_empty(),
        "a twin was chosen without a decision"
    );
    let site = p.rows("MATCH (c:CallSite) WHERE c.callee_name = 'pick' RETURN c.unresolved_reason");
    assert_eq!(site, [["cfg_twins"]], "{site:?}");

    let manifest_path = p.graph.parent().unwrap().join("manifest.json");
    indexer::write_full_manifest(&p.root, &manifest_path, &indexer::IndexOptions::default())
        .expect("manifest");
    let prior = indexer::manifest::load(&manifest_path).expect("manifest loads");
    indexer::index_incremental(
        &p.root,
        &p.graph,
        &manifest_path,
        &indexer::IndexOptions::default(),
        &prior,
    )
    .expect("incremental refresh");
    p.resolve();
    let edges = p.calls_into("pick");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(
        edges[0][1].ends_with("#cfg(not(feature=fast))"),
        "{edges:?}"
    );
}

/// The graph of the first part has `cfg_gate` and the marker but no
/// `cfg_active`, and passes the guard. An incremental refresh that REPARSES a
/// Rust file re-inserts its nodes with a `cfg_active` value, so the column has to
/// exist before the nodes are written, not only when the activity pass runs.
#[test]
fn an_incremental_refresh_that_reparses_a_file_repairs_a_graph_without_the_column() {
    let p = Project::new("fast = []", &[("src/lib.rs", PICK)]);
    let store = p.store();
    for label in ai_architect_mcp::graph_store::CFG_GATE_LABELS {
        store
            .execute_query(&format!("ALTER TABLE {label} DROP cfg_active"))
            .expect("drop the column to imitate the graph of the first part");
    }
    drop(store);
    let manifest_path = p.graph.parent().unwrap().join("manifest.json");
    indexer::write_full_manifest(&p.root, &manifest_path, &indexer::IndexOptions::default())
        .expect("manifest");
    fs::write(
        p.root.join("src/lib.rs"),
        format!("{PICK}\npub fn added() {{}}\n"),
    )
    .unwrap();
    let prior = indexer::manifest::load(&manifest_path).expect("manifest loads");
    indexer::index_incremental(
        &p.root,
        &p.graph,
        &manifest_path,
        &indexer::IndexOptions::default(),
        &prior,
    )
    .expect("the refresh of a graph from the first part must not fail");
    assert_eq!(
        p.activity("Function", "pick"),
        [
            pair("feature=fast", "inactive"),
            pair("not(feature=fast)", "active")
        ]
    );
    p.resolve();
    let edges = p.calls_into("pick");
    assert_eq!(edges.len(), 1, "{edges:?}");
}

/// A twin file that is deleted takes its twins and the edges into them with it;
/// the caller in another file is left open, not pointed at a node that is gone.
#[test]
fn deleting_the_file_that_holds_the_twins_leaves_the_caller_open() {
    let p = Project::new(
        "fast = []",
        &[
            (
                "src/lib.rs",
                "mod twins;\npub fn caller() -> u32 {\n    twins::pick()\n}\n",
            ),
            (
                "src/twins.rs",
                "#[cfg(feature = \"fast\")]\npub fn pick() -> u32 {\n    1\n}\n\
                 #[cfg(not(feature = \"fast\"))]\npub fn pick() -> u32 {\n    2\n}\n",
            ),
        ],
    );
    assert_eq!(p.calls_into("pick").len(), 1, "{:?}", p.calls_into("pick"));
    let manifest_path = p.graph.parent().unwrap().join("manifest.json");
    indexer::write_full_manifest(&p.root, &manifest_path, &indexer::IndexOptions::default())
        .expect("manifest");
    fs::remove_file(p.root.join("src/twins.rs")).unwrap();
    fs::write(
        p.root.join("src/lib.rs"),
        "pub fn caller() -> u32 {\n    0\n}\n",
    )
    .unwrap();
    let prior = indexer::manifest::load(&manifest_path).expect("manifest loads");
    indexer::index_incremental(
        &p.root,
        &p.graph,
        &manifest_path,
        &indexer::IndexOptions::default(),
        &prior,
    )
    .expect("incremental refresh");
    p.resolve();
    assert!(p
        .rows("MATCH (f:Function) WHERE f.name = 'pick' RETURN f.id")
        .is_empty());
    assert!(p.calls_into("pick").is_empty());
}
