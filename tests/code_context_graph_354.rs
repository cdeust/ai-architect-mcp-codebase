//! Issue #354 at the graph layer: what `File.target_context` and
//! `Function.code_context` hold for real Cargo packages (a workspace, a lib under
//! a nested path, a target added by editing `Cargo.toml`), that an incremental
//! refresh keeps them true, and that a graph written before the columns is
//! refused for incremental writes and read as `absent`.
use ai_architect_mcp::{clustering, graph_store::GraphStore, indexer, resolver};
use std::fs;
use std::path::{Path, PathBuf};

const WORKSPACE: &str = "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\nresolver = \"2\"\n";

fn package(name: &str, extra: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{extra}")
}

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    graph: PathBuf,
}

impl Project {
    fn new(files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        for (name, text) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
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

    /// `File.target_context` of `file`.
    fn target_context(&self, file: &str) -> String {
        self.rows(&format!(
            "MATCH (f:File) WHERE f.id = '{file}' RETURN f.target_context"
        ))[0][0]
            .clone()
    }

    fn incremental(&self) -> Result<(), String> {
        let manifest_path = self.graph.parent().unwrap().join("manifest.json");
        let prior = indexer::manifest::load(&manifest_path).expect("manifest loads");
        indexer::index_incremental(
            &self.root,
            &self.graph,
            &manifest_path,
            &indexer::IndexOptions::default(),
            &prior,
        )
        .map(|_| ())
    }

    fn write_manifest(&self) {
        let manifest_path = self.graph.parent().unwrap().join("manifest.json");
        indexer::write_full_manifest(
            &self.root,
            &manifest_path,
            &indexer::IndexOptions::default(),
        )
        .expect("manifest");
    }
}

fn contexts_of_callers(store: &GraphStore, target: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = clustering::get_impact(store, target)
        .expect("impact")
        .callers
        .into_iter()
        .map(|c| (c.id, c.context))
        .collect();
    out.sort();
    out
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

/// Two members of a workspace and a library whose entry file is not under `src/`.
fn workspace_files() -> Vec<(&'static str, String)> {
    vec![
        ("Cargo.toml", WORKSPACE.to_string()),
        ("crates/a/Cargo.toml", package("a", "")),
        (
            "crates/a/src/lib.rs",
            "pub fn core() -> u32 { 1 }\n#[cfg(test)]\nmod tests;\n".to_string(),
        ),
        (
            "crates/a/src/tests.rs",
            "fn t() { crate::core(); }\n".to_string(),
        ),
        (
            "crates/a/tests/it.rs",
            "mod support;\n#[test]\nfn it() { a::core(); }\n".to_string(),
        ),
        (
            "crates/a/tests/support/mod.rs",
            "pub fn s() { a::core(); }\n".to_string(),
        ),
        (
            "crates/b/Cargo.toml",
            package("b", "\n[lib]\npath = \"lib/entry.rs\"\n"),
        ),
        (
            "crates/b/lib/entry.rs",
            "pub mod util;\npub fn from_b() -> u32 { a::core() }\n".to_string(),
        ),
        (
            "crates/b/lib/util.rs",
            "pub fn u() -> u32 { a::core() }\n".to_string(),
        ),
        (
            "crates/b/examples/demo.rs",
            "fn main() { let _ = a::core(); }\n".to_string(),
        ),
    ]
}

#[test]
fn a_workspace_gives_each_file_the_context_of_the_target_that_reaches_it() {
    let files = workspace_files();
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let p = Project::new(&refs);
    for (file, expected) in [
        ("crates/a/src/lib.rs", "production"),
        ("crates/a/src/tests.rs", "test"),
        ("crates/a/tests/it.rs", "test"),
        ("crates/a/tests/support/mod.rs", "test"),
        ("crates/b/lib/entry.rs", "production"),
        ("crates/b/lib/util.rs", "production"),
        ("crates/b/examples/demo.rs", "example"),
    ] {
        assert_eq!(p.target_context(file), expected, "{file}");
    }
}

#[test]
fn callers_across_members_carry_their_own_context() {
    let files = workspace_files();
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let p = Project::new(&refs);
    let callers = contexts_of_callers(&p.store(), "crates/a/src/lib.rs::core");
    for (id, context) in [
        ("crates/a/src/tests.rs::t", "test"),
        ("crates/a/tests/it.rs::it", "test"),
        ("crates/a/tests/support/mod.rs::s", "test"),
        ("crates/b/lib/entry.rs::from_b", "production"),
        ("crates/b/lib/util.rs::u", "production"),
        ("crates/b/examples/demo.rs::main", "example"),
    ] {
        assert!(
            callers.contains(&pair(id, context)),
            "{id} should be {context}: {callers:?}"
        );
    }
}

/// A `src/tests.rs` that `lib.rs` declares as a plain `mod tests;` is production:
/// the name proves nothing.
#[test]
fn a_file_called_tests_declared_as_a_plain_module_is_production() {
    let p = Project::new(&[
        ("Cargo.toml", &package("plain", "")),
        ("src/lib.rs", "mod tests;\npub fn core() -> u32 { 1 }\n"),
        ("src/tests.rs", "pub fn helper() { crate::core(); }\n"),
    ]);
    assert_eq!(p.target_context("src/tests.rs"), "production");
    let callers = contexts_of_callers(&p.store(), "src/lib.rs::core");
    assert_eq!(callers, [pair("src/tests.rs::helper", "production")]);
}

/// Editing `Cargo.toml` changes the class of a file that did not change: the
/// incremental refresh has to rewrite the column, not carry it.
#[test]
fn a_cargo_toml_edit_flips_the_context_of_an_untouched_file() {
    let p = Project::new(&[
        ("Cargo.toml", &package("flip", "")),
        ("src/lib.rs", "pub fn core() -> u32 { 1 }\n"),
        ("src/t.rs", "fn t() { flip::core(); }\n"),
    ]);
    assert_eq!(
        p.target_context("src/t.rs"),
        "",
        "no target reaches it: nothing is decided"
    );
    p.write_manifest();
    fs::write(
        p.root.join("Cargo.toml"),
        package("flip", "\n[[test]]\nname = \"t\"\npath = \"src/t.rs\"\n"),
    )
    .unwrap();
    p.incremental().expect("incremental refresh");
    assert_eq!(p.target_context("src/t.rs"), "test");
}

#[test]
fn a_tree_without_a_cargo_map_uses_the_source_alone() {
    let p = Project::new(&[(
        "lib.rs",
        "pub fn core() -> u32 { 1 }\npub fn user() -> u32 { core() }\n\
             #[test]\nfn a_test() { core(); }\n",
    )]);
    let store = p.store();
    assert_eq!(store.code_context_basis(), "source_only");
    assert_eq!(
        contexts_of_callers(&store, "lib.rs::core"),
        [
            pair("lib.rs::a_test", "test"),
            pair("lib.rs::user", "unknown")
        ]
    );
    let impact = clustering::get_impact(&store, "lib.rs::core").unwrap();
    let summary = clustering::summarize_caller_contexts(&impact.callers);
    assert_eq!(
        summary.production_total, 1,
        "unknown counts as production: only the proven test is taken away"
    );
}

#[test]
fn a_full_index_writes_both_markers() {
    let p = Project::new(&[
        ("Cargo.toml", &package("marks", "")),
        ("src/lib.rs", "pub fn f() {}\n"),
    ]);
    let mut values: Vec<String> = p
        .rows(
            "MATCH (m:GraphMarker) WHERE m.id IN ['cfg_canonical_form', 'code_context_form'] \
             RETURN m.id, m.value",
        )
        .into_iter()
        .map(|r| format!("{}={}", r[0], r[1]))
        .collect();
    values.sort();
    assert_eq!(values, ["cfg_canonical_form=2", "code_context_form=1"]);
    assert!(p.store().has_code_context());
}

fn old_shape(graph: &Path) {
    let store = GraphStore::open_or_create(graph).unwrap();
    store
        .execute_query("MATCH (m:GraphMarker {id: 'code_context_form'}) DELETE m")
        .unwrap();
    for label in ["Function", "Method"] {
        store
            .execute_query(&format!("ALTER TABLE {label} DROP code_context"))
            .unwrap();
    }
    store
        .execute_query("ALTER TABLE File DROP target_context")
        .unwrap();
}

/// A graph from the first part of #353 or #365 has the twin metadata but no
/// code context. `get_impact` reads it as `absent` and every caller `unknown`;
/// an incremental refresh and a library full index over it are refused, since an
/// unchanged file would keep a stale empty value.
#[test]
fn a_graph_written_before_the_columns_is_read_as_absent_and_refused_for_writes() {
    let p = Project::new(&[
        ("Cargo.toml", &package("old", "")),
        (
            "src/lib.rs",
            "pub fn core() -> u32 { 1 }\n#[test]\nfn t() { core(); }\n",
        ),
    ]);
    p.write_manifest();
    old_shape(&p.graph);
    let store = p.store();
    assert!(!store.has_code_context());
    assert_eq!(store.code_context_basis(), "absent");
    assert_eq!(
        contexts_of_callers(&store, "src/lib.rs::core"),
        [pair("src/lib.rs::t", "unknown")]
    );
    drop(store);
    let refused = p
        .incremental()
        .expect_err("an incremental write is refused");
    assert!(refused.contains("full reindex required"), "{refused}");
    let full = match indexer::index_codebase(&p.root, &p.graph) {
        Ok(_) => panic!("a full index over an old graph must be refused"),
        Err(message) => message,
    };
    assert!(full.contains("full reindex required"), "{full}");
}
