//! Issue #366, part B: twin items spread over two files that one `mod`
//! declaration selects under exclusive `#[cfg]` predicates (`#[path]` pairs,
//! `cfg_attr(pred, path = ..)`). The files have their own ids, so the twins are
//! told apart by the module path the declarations give them, and a call to
//! them is resolved to the file the default build compiles, or left open with
//! the reason `cfg_twins` when the build does not decide.
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::fs;
use std::path::PathBuf;

fn manifest(features: &str) -> String {
    format!(
        "[package]\nname = \"fx366\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\n{features}\n"
    )
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
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let graph = tmp.path().join("graph");
        indexer::index_codebase(&root, &graph).expect("index");
        let store = GraphStore::open_or_create(&graph).expect("open graph");
        resolver::resolve_graph(&store).expect("resolve");
        drop(store);
        Project {
            _tmp: tmp,
            root,
            graph,
        }
    }

    fn resolve(&self) {
        let store = GraphStore::open_or_create(&self.graph).expect("open graph");
        resolver::resolve_graph(&store).expect("resolve");
    }

    fn rows(&self, cypher: &str) -> Vec<Vec<String>> {
        GraphStore::open_or_create(&self.graph)
            .expect("open graph")
            .execute_query(cypher)
            .expect(cypher)
            .rows
    }

    /// `(target id, method)` of every per-site row of the calls named `callee`
    /// made from the function `caller`.
    fn targets(&self, caller: &str, callee: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .rows(&format!(
                "MATCH (c:CallSite)-[r:Calls_CallSite_Function]->(t:Function) \
                 WHERE c.callee_name ENDS WITH '{callee}' AND c.id CONTAINS '::{caller}::call@' \
                 RETURN t.id, r.resolution_method"
            ))
            .into_iter()
            .map(|r| (r[0].clone(), r[1].clone()))
            .collect();
        out.sort();
        out
    }

    /// `(is_resolved, unresolved_reason)` of the calls named `callee` made from
    /// the function `caller`.
    fn sites(&self, caller: &str, callee: &str) -> Vec<(String, String)> {
        self.rows(&format!(
            "MATCH (c:CallSite) WHERE c.callee_name ENDS WITH '{callee}' \
             AND c.id CONTAINS '::{caller}::call@' RETURN c.is_resolved, c.unresolved_reason"
        ))
        .into_iter()
        .map(|r| (r[0].to_lowercase(), r[1].clone()))
        .collect()
    }
}

fn row(target: &str, method: &str) -> (String, String) {
    (target.to_string(), method.to_string())
}

const PATH_PAIR: &str = "#[cfg(feature = \"fast\")]
#[path = \"fast.rs\"]
mod imp;

#[cfg(not(feature = \"fast\"))]
#[path = \"slow.rs\"]
mod imp;

pub fn caller() -> u32 {
    imp::pick()
}
";

const FAST: &str = "pub fn pick() -> u32 {\n    1\n}\n";
const SLOW: &str = "pub fn pick() -> u32 {\n    2\n}\n";

#[test]
fn a_path_pair_resolves_to_the_file_the_default_features_compile() {
    let p = Project::new(
        "fast = []",
        &[
            ("src/lib.rs", PATH_PAIR),
            ("src/fast.rs", FAST),
            ("src/slow.rs", SLOW),
        ],
    );
    assert_eq!(
        p.targets("caller", "pick"),
        [row("src/slow.rs::pick", "cfg-selected")]
    );
    let on = Project::new(
        "default = [\"fast\"]\nfast = []",
        &[
            ("src/lib.rs", PATH_PAIR),
            ("src/fast.rs", FAST),
            ("src/slow.rs", SLOW),
        ],
    );
    assert_eq!(
        on.targets("caller", "pick"),
        [row("src/fast.rs::pick", "cfg-selected")]
    );
}

const OS_PAIR: &str = "#[cfg(unix)]
#[path = \"unix.rs\"]
mod imp;

#[cfg(windows)]
#[path = \"windows.rs\"]
mod imp;

pub fn caller() -> u32 {
    imp::pick()
}
";

#[test]
fn a_path_pair_the_build_does_not_decide_stays_open_with_the_twin_reason() {
    let p = Project::new(
        "",
        &[
            ("src/lib.rs", OS_PAIR),
            ("src/unix.rs", FAST),
            ("src/windows.rs", SLOW),
        ],
    );
    assert!(
        p.targets("caller", "pick").is_empty(),
        "no edge to either file"
    );
    assert_eq!(
        p.sites("caller", "pick"),
        [("false".to_string(), "cfg_twins".to_string())]
    );
}

const CFG_ATTR: &str = "#[cfg_attr(feature = \"fast\", path = \"fast.rs\")]
#[cfg_attr(not(feature = \"fast\"), path = \"slow.rs\")]
mod imp;

pub fn caller() -> u32 {
    imp::pick()
}
";

#[test]
fn a_cfg_attr_path_pair_resolves_to_the_file_the_default_features_compile() {
    let p = Project::new(
        "fast = []",
        &[
            ("src/lib.rs", CFG_ATTR),
            ("src/fast.rs", FAST),
            ("src/slow.rs", SLOW),
        ],
    );
    assert_eq!(
        p.targets("caller", "pick"),
        [row("src/slow.rs::pick", "cfg-selected")]
    );
}

const NESTED_LIB: &str = "#[cfg(unix)]
#[path = \"sys/unix/mod.rs\"]
pub mod imp;

#[cfg(not(unix))]
#[path = \"sys/other/mod.rs\"]
pub mod imp;

pub fn caller() -> u32 {
    imp::helpers::h()
}
";

const NESTED_MOD: &str = "pub mod helpers;

pub fn run() -> u32 {
    helpers::h()
}
";

#[test]
fn nested_modules_of_twin_files_are_twins_and_follow_the_callers_file_gate() {
    let p = Project::new(
        "",
        &[
            ("src/lib.rs", NESTED_LIB),
            ("src/sys/unix/mod.rs", NESTED_MOD),
            (
                "src/sys/unix/helpers.rs",
                FAST.replace("pick", "h").as_str(),
            ),
            ("src/sys/other/mod.rs", NESTED_MOD),
            (
                "src/sys/other/helpers.rs",
                SLOW.replace("pick", "h").as_str(),
            ),
        ],
    );
    // From the crate root, `unix` is not decided: no edge, the twin reason.
    assert!(p.targets("caller", "h").is_empty());
    assert_eq!(
        p.sites("caller", "h"),
        [("false".to_string(), "cfg_twins".to_string())]
    );
    // From inside a twin file, the file's own gate decides: `run` in the unix
    // file exists only when `unix` holds, so it reaches the unix helper.
    let from_unix: Vec<_> = p
        .targets("run", "h")
        .into_iter()
        .filter(|(t, _)| t.starts_with("src/sys/"))
        .collect();
    assert!(
        from_unix.contains(&row("src/sys/unix/helpers.rs::h", "cfg-selected")),
        "{from_unix:?}"
    );
    assert!(
        from_unix.contains(&row("src/sys/other/helpers.rs::h", "cfg-selected")),
        "{from_unix:?}"
    );
    assert_eq!(from_unix.len(), 2, "one edge per caller: {from_unix:?}");
}

const DIFFERENT_NAMES: &str = "#[cfg(feature = \"fast\")]
mod a;
mod b;

pub fn caller() -> u32 {
    pick()
}
";

#[test]
fn a_gated_module_beside_a_differently_named_one_is_not_a_twin() {
    let p = Project::new(
        "fast = []",
        &[
            ("src/lib.rs", DIFFERENT_NAMES),
            ("src/a.rs", FAST),
            ("src/b.rs", SLOW),
        ],
    );
    assert!(
        !p.targets("caller", "pick")
            .iter()
            .any(|(_, m)| m == "cfg-selected"),
        "different module paths are not twins"
    );
    assert!(
        !p.sites("caller", "pick")
            .iter()
            .any(|(_, reason)| reason == "cfg_twins"),
        "an ordinary ambiguity keeps its own reason"
    );
}

/// The file facts are rewritten on every pass: a `Cargo.toml` edit that turns
/// `fast` on moves the edge to the other file without reparsing either.
#[test]
fn an_incremental_refresh_follows_a_features_change_across_files() {
    let p = Project::new(
        "fast = []",
        &[
            ("src/lib.rs", PATH_PAIR),
            ("src/fast.rs", FAST),
            ("src/slow.rs", SLOW),
        ],
    );
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
    p.resolve();
    assert_eq!(
        p.targets("caller", "pick"),
        [row("src/fast.rs::pick", "cfg-selected")]
    );
}

/// Writes what an earlier run left: a language-server row, caller-level and
/// per-site, from `caller`'s call to `pick` into the compiled-out `fast.rs`.
/// Unless `keep_static_row`, the static rows go and the site stays resolved.
fn plant_stale_lsp_row(p: &Project, keep_static_row: bool) {
    let store = GraphStore::open_or_create(&p.graph).expect("open graph");
    let site = p.rows(
        "MATCH (c:CallSite) WHERE c.callee_name ENDS WITH 'pick' \
         AND c.id CONTAINS '::caller::call@' RETURN c.id",
    )[0][0]
        .clone();
    if !keep_static_row {
        store.reset_call_rows("cfg-selected").expect("static rows");
        let mark = format!("MATCH (c:CallSite {{id: '{site}'}}) SET c.is_resolved = true");
        store.execute_query(&mark).expect("mark resolved");
    }
    let props = [
        ("confidence", "0.9"),
        ("resolution_method", "'lsp-definition'"),
    ];
    for (rel, from) in [
        ("Calls_CallSite_Function", site.as_str()),
        ("Calls_Function_Function", "src/lib.rs::caller"),
    ] {
        store
            .insert_edge_if_absent(rel, from, "src/fast.rs::pick", &props)
            .expect("stale row");
    }
}

/// A language-server row an earlier run wrote to the file twin the build now
/// compiles out (review of #385). The ids of the file twins carry no
/// `#cfg(..)`, so the reset must find the row through the file's facts; else
/// the stale row survives next to the one the static pass writes, and the site
/// has two targets.
#[test]
fn a_stale_language_server_row_to_a_compiled_out_twin_file_is_reset() {
    for keep_static_row in [false, true] {
        let p = Project::new(
            "fast = []",
            &[
                ("src/lib.rs", PATH_PAIR),
                ("src/fast.rs", FAST),
                ("src/slow.rs", SLOW),
            ],
        );
        plant_stale_lsp_row(&p, keep_static_row);
        p.resolve();
        assert_eq!(
            p.targets("caller", "pick"),
            [row("src/slow.rs::pick", "cfg-selected")],
            "one target per site, the compiled file's (static row kept: {keep_static_row})"
        );
        let caller = p.rows(
            "MATCH (a:Function {id: 'src/lib.rs::caller'})-[r:Calls_Function_Function]->(t) \
             RETURN t.id",
        );
        assert!(
            !caller.iter().any(|r| r[0] == "src/fast.rs::pick"),
            "the caller keeps no row to the compiled-out file: {caller:?}"
        );
    }
}
