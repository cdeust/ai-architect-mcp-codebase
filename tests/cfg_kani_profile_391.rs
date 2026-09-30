//! Issue #391: a plain `cargo build` never sets `kani`, `miri`, `doc` or
//! `doctest`, so under the default profile the `cfg(not(kani))` twin is compiled
//! and the `cfg(kani)` twin is not. `unix`, `test` and `target_os` stay unknown.
//!
//! The fixture is the shape dy-wcet v4.1.6 has at `src/lib.rs:88/96, 114/135,
//! 141/147`: three constants, each defined once under `cfg(kani)` and once under
//! `cfg(not(kani))`. Every fixture is a real Cargo package, so `cargo metadata`
//! gives the package's features and the indexer writes `cfg_active`.
use ai_architect_mcp::{clustering, graph_store::GraphStore, indexer, resolver};
use std::fs;
use std::path::PathBuf;

const DY_WCET_SHAPE: &str = "\
#[cfg(kani)]\npub const BUSY_PERIOD_CAP: u64 = 4;\n\
#[cfg(not(kani))]\npub const BUSY_PERIOD_CAP: u64 = 1_000_000;\n\n\
#[cfg(kani)]\npub const ITERATION_CAP: u32 = 3;\n\
#[cfg(not(kani))]\npub const ITERATION_CAP: u32 = 10_000;\n\n\
#[cfg(kani)]\npub const MAX_TASKS: usize = 2;\n\
#[cfg(not(kani))]\npub const MAX_TASKS: usize = 64;\n";

const PICK: &str = "\
#[cfg(kani)]\npub fn pick() -> u32 {\n    1\n}\n\n\
#[cfg(not(kani))]\npub fn pick() -> u32 {\n    2\n}\n\n\
#[cfg(unix)]\npub fn os() -> u32 {\n    1\n}\n\n\
#[cfg(not(unix))]\npub fn os() -> u32 {\n    2\n}\n\n\
pub fn caller() -> u32 {\n    pick() + os()\n}\n";

struct Project {
    _tmp: tempfile::TempDir,
    graph: PathBuf,
}

impl Project {
    fn new(source: &str) -> Self {
        Self::with_files(&[("src/lib.rs", source)])
    }

    fn with_files(files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("crate");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        for (name, text) in files {
            fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
            fs::write(root.join(name), text).unwrap();
        }
        let graph = tmp.path().join("graph");
        indexer::index_codebase(&root, &graph).expect("index");
        let project = Project { _tmp: tmp, graph };
        resolver::resolve_graph(&project.store()).expect("resolve");
        project
    }

    fn store(&self) -> GraphStore {
        GraphStore::open_or_create(&self.graph).expect("open graph")
    }

    fn rows(&self, cypher: &str) -> Vec<Vec<String>> {
        self.store().execute_query(cypher).expect(cypher).rows
    }

    /// `(cfg_gate, cfg_active)` of every node of `label` named `name`, sorted.
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

fn pair(gate: &str, active: &str) -> (String, String) {
    (gate.to_string(), active.to_string())
}

/// The issue's reproduction: 3 twin sets, 6 members, none `unknown` any more.
#[test]
fn the_three_dy_wcet_twin_sets_are_decided_by_the_default_profile() {
    let p = Project::new(DY_WCET_SHAPE);
    for name in ["BUSY_PERIOD_CAP", "ITERATION_CAP", "MAX_TASKS"] {
        assert_eq!(
            p.activity("Constant", name),
            [pair("kani", "inactive"), pair("not(kani)", "active")],
            "{name}"
        );
    }
}

/// The call reaches the `not(kani)` twin, chosen by the build profile; the
/// `unix` twins are not decided by the source, so their call stays open.
#[test]
fn a_call_reaches_the_not_kani_twin_and_unix_twins_stay_unknown() {
    let p = Project::new(PICK);
    assert_eq!(
        p.activity("Function", "os"),
        [pair("not(unix)", "unknown"), pair("unix", "unknown")]
    );
    let edges = p.rows(
        "MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) WHERE a.name = 'caller' \
         RETURN b.qualified_name, r.resolution_method",
    );
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(edges[0][0].ends_with("pick#cfg(not(kani))"), "{edges:?}");
    assert_eq!(edges[0][1], "cfg-selected", "{edges:?}");
    let open = p.rows(
        "MATCH (c:CallSite) WHERE c.callee_name = 'os' RETURN c.is_resolved, c.unresolved_reason",
    );
    assert_eq!(open[0][0].to_lowercase(), "false", "{open:?}");
    assert_eq!(open[0][1], "cfg_twins", "{open:?}");
}

/// Issue #420: the module gate is read under the same profile as the item gates.
/// `#[cfg(kani)] mod proofs;` is compiled out of a default build, so no twin in
/// it is compiled: the `cfg(not(kani))` one is not `active` merely because its
/// own gate holds.
#[test]
fn twins_in_a_kani_gated_module_are_inactive_under_the_default_profile() {
    let twins = "#[cfg(kani)]\npub fn g() -> u32 {\n    1\n}\n\n\
#[cfg(not(kani))]\npub fn g() -> u32 {\n    2\n}\n";
    let p = Project::with_files(&[
        (
            "src/lib.rs",
            "#[cfg(kani)]\nmod proofs;\n\npub fn root() {}\n",
        ),
        ("src/proofs.rs", twins),
    ]);
    assert_eq!(
        p.activity("Function", "g"),
        [pair("kani", "inactive"), pair("not(kani)", "inactive")]
    );
}

/// The mirror: under `#[cfg(not(kani))] mod plain;` the module is compiled, so
/// the item gates decide.
#[test]
fn twins_in_a_not_kani_module_follow_their_own_gates() {
    let twins = "#[cfg(kani)]\npub fn g() -> u32 {\n    1\n}\n\n\
#[cfg(not(kani))]\npub fn g() -> u32 {\n    2\n}\n";
    let p = Project::with_files(&[
        (
            "src/lib.rs",
            "#[cfg(not(kani))]\nmod plain;\n\npub fn root() {}\n",
        ),
        ("src/plain.rs", twins),
    ]);
    assert_eq!(
        p.activity("Function", "g"),
        [pair("kani", "inactive"), pair("not(kani)", "active")]
    );
}

/// Issue #423: a helper in a `#[cfg(kani)]` module has no `#[kani::proof]`, so
/// its own marker is empty and its file decides. That file is compiled out of a
/// default build and is Kani-only code, the way `#[cfg(test)] mod tests;` is
/// test-only code: `get_impact` must not count its call as a production call.
/// The shape is dy-wcet v4.1.6 `kani/response_bounds.rs`, placed by `#[path]`.
#[test]
fn a_helper_in_a_kani_gated_module_is_not_a_production_caller() {
    let lib = "\
pub struct Set;\n\n\
impl Set {\n    pub fn respond(&self) -> u32 {\n        1\n    }\n}\n\n\
pub fn from_production(s: &Set) -> u32 {\n    s.respond()\n}\n\n\
#[cfg(kani)]\n#[path = \"../kani/bounds.rs\"]\nmod bounds;\n";
    let bounds = "\
use crate::Set;\n\n\
fn helper(s: &Set) -> u32 {\n    s.respond()\n}\n\n\
#[kani::proof]\nfn harness() {\n    let _ = helper(&Set);\n}\n";
    let p = Project::with_files(&[("src/lib.rs", lib), ("kani/bounds.rs", bounds)]);
    let mut callers: Vec<(String, String)> =
        clustering::get_impact(&p.store(), "src/lib.rs::Set::respond")
            .expect("impact")
            .callers
            .into_iter()
            .map(|c| (c.id, c.context))
            .collect();
    callers.sort();
    assert_eq!(
        callers,
        [
            pair("kani/bounds.rs::helper", "proof"),
            pair("src/lib.rs::from_production", "production"),
        ]
    );
    assert_eq!(
        p.rows(
            "MATCH (f:File) WHERE f.id = 'kani/bounds.rs' RETURN f.cfg_active, f.target_context"
        ),
        [["inactive", "proof"]]
    );
}
