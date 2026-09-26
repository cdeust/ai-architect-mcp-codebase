// receiver_import_evidence_348 — issues #348 and #349: a receiver typed from the
// return type of a free function whose type name is shown only by an explicit
// `use` of a path outside `crate`, `self` and `super` is kept only when that
// path starts with a library crate of the analysed repository. The evidence
// comes from `cargo metadata` (package and `[lib]` names, workspace members),
// never from the name of the type: the last-segment lookup would otherwise
// match an unrelated repository type of the same name.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::{indexer, resolver};
use std::fs;
use std::process::Command;
mod common;
use common::TempDirExt;

/// The `answer` type every fixture owns: exactly one candidate in the repo.
const OWNER: &str =
    "pub struct Set;\nimpl Set {\n    pub fn answer(&self) -> u32 {\n        1\n    }\n}\n";

/// `use <import>;` then a free function returning `Set` and a call through it.
fn user(import: &str, var: &str) -> String {
    format!(
        "{import}\n\npub fn make_{var}() -> Set {{\n    todo!()\n}}\n\n\
         pub fn run_{var}() -> u32 {{\n    let {var} = make_{var}();\n    {var}.answer()\n}}\n"
    )
}

fn cargo_available() -> bool {
    Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Writes `files` (relative path, text) under a fresh root and indexes it,
/// without resolving.
fn index_only(prefix: &str, files: &[(&str, String)]) -> (GraphStore, common::TestTempDir) {
    let tmp = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let root = tmp.path().join("fixture");
    for (rel, text) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write fixture file");
    }
    let graph_dir = tmp.path().join("graph");
    indexer::index_codebase(&root, &graph_dir).expect("index");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    (store, tmp)
}

fn index_and_resolve(prefix: &str, files: &[(&str, String)]) -> (GraphStore, common::TestTempDir) {
    let (store, tmp) = index_only(prefix, files);
    resolver::resolve_graph(&store).expect("resolve");
    (store, tmp)
}

/// Target ids of the per-site rows of the call `<var>.answer`.
fn targets(store: &GraphStore, var: &str) -> Vec<String> {
    store
        .execute_query(&format!(
            "MATCH (cs:CallSite)-[r:Calls_CallSite_Method]->(t) \
             WHERE cs.callee_name = '{var}.answer' RETURN t.id"
        ))
        .expect("query rows")
        .rows
        .into_iter()
        .map(|r| r[0].clone())
        .collect()
}

fn manifest(name: &str, extra: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{extra}\n")
}

/// The repository holds ONE unrelated `Set` with an `answer`; the return type
/// comes from a crate the repository does not hold.
#[test]
fn a_type_imported_from_a_foreign_crate_gets_no_edge_to_a_namesake() {
    if !cargo_available() {
        return;
    }
    let (store, _tmp) = index_and_resolve(
        "import_evidence_foreign_",
        &[
            ("Cargo.toml", manifest("mine", "")),
            ("src/lib.rs", "pub mod other;\npub mod user;\n".into()),
            ("src/other.rs", OWNER.into()),
            ("src/user.rs", user("use foreign::Set;", "x")),
        ],
    );
    assert!(
        targets(&store, "x").is_empty(),
        "{:?}",
        targets(&store, "x")
    );
}

/// A crate that exists in the repository under another name is not `foreign`.
#[test]
fn an_external_crate_of_a_different_name_is_declined_beside_the_own_crate() {
    if !cargo_available() {
        return;
    }
    let (store, _tmp) = index_and_resolve(
        "import_evidence_other_name_",
        &[
            ("Cargo.toml", manifest("mine", "")),
            ("src/lib.rs", "pub mod shapes;\npub mod user;\n".into()),
            ("src/shapes.rs", OWNER.into()),
            ("src/user.rs", user("use mine_other::Set;", "x")),
        ],
    );
    assert!(targets(&store, "x").is_empty());
}

#[test]
fn a_type_imported_through_crate_is_accepted() {
    if !cargo_available() {
        return;
    }
    let (store, _tmp) = index_and_resolve(
        "import_evidence_crate_",
        &[
            ("Cargo.toml", manifest("mine", "")),
            ("src/lib.rs", "pub mod shapes;\npub mod user;\n".into()),
            ("src/shapes.rs", OWNER.into()),
            ("src/user.rs", user("use crate::shapes::Set;", "x")),
        ],
    );
    assert_eq!(targets(&store, "x"), vec!["src/shapes.rs::Set::answer"]);
}

/// The measured crate's shape: an integration test imports the package's own
/// library by name, which is the package name with hyphens as underscores.
#[test]
fn a_hyphenated_package_is_imported_by_its_underscored_name() {
    if !cargo_available() {
        return;
    }
    let (store, _tmp) = index_and_resolve(
        "import_evidence_hyphen_",
        &[
            ("Cargo.toml", manifest("my-crate", "")),
            ("src/lib.rs", OWNER.into()),
            ("tests/it.rs", user("use my_crate::Set;", "x")),
        ],
    );
    assert_eq!(targets(&store, "x"), vec!["src/lib.rs::Set::answer"]);
}

/// `[lib] name` decides the crate name; the package name no longer works.
#[test]
fn a_lib_name_override_is_the_crate_name_and_the_package_name_is_not() {
    if !cargo_available() {
        return;
    }
    let (store, _tmp) = index_and_resolve(
        "import_evidence_lib_name_",
        &[
            (
                "Cargo.toml",
                manifest("my-crate", "\n[lib]\nname = \"alt\""),
            ),
            ("src/lib.rs", OWNER.into()),
            ("tests/by_alt.rs", user("use alt::Set;", "x")),
            ("tests/by_package.rs", user("use my_crate::Set;", "y")),
        ],
    );
    assert_eq!(targets(&store, "x"), vec!["src/lib.rs::Set::answer"]);
    assert!(
        targets(&store, "y").is_empty(),
        "{:?}",
        targets(&store, "y")
    );
}

/// Two members of a workspace importing each other.
#[test]
fn workspace_members_importing_each_other_are_crates_of_the_repository() {
    if !cargo_available() {
        return;
    }
    let (store, _tmp) = index_and_resolve(
        "import_evidence_workspace_",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n".into(),
            ),
            ("a/Cargo.toml", manifest("a", "")),
            ("a/src/lib.rs", OWNER.into()),
            (
                "b/Cargo.toml",
                manifest("b", "\n[dependencies]\na = { path = \"../a\" }"),
            ),
            ("b/src/lib.rs", user("use a::Set;", "x")),
        ],
    );
    assert_eq!(targets(&store, "x"), vec!["a/src/lib.rs::Set::answer"]);
}

/// No Cargo.toml: nothing shows that any crate belongs to the repository.
#[test]
fn a_repository_without_cargo_metadata_declines_a_type_from_an_external_path() {
    let (store, _tmp) = index_and_resolve(
        "import_evidence_no_cargo_",
        &[
            ("lib.rs", "pub mod shapes;\npub mod user;\n".into()),
            ("shapes.rs", OWNER.into()),
            ("user.rs", user("use mine::Set;", "x")),
        ],
    );
    assert!(targets(&store, "x").is_empty());
}

/// The decision follows the crate evidence recorded by the latest index pass,
/// never the one recorded when the file was parsed (issue #358): the hint keeps
/// its mark, and a resolve under facts that no longer name the crate declines it.
#[test]
fn an_import_hint_is_decided_by_the_latest_recorded_evidence() {
    if !cargo_available() {
        return;
    }
    let (store, _tmp) = index_only(
        "import_evidence_latest_",
        &[
            ("Cargo.toml", manifest("mine", "")),
            ("src/lib.rs", "pub mod shapes;\npub mod user;\n".into()),
            ("src/shapes.rs", OWNER.into()),
            ("src/user.rs", user("use mine::shapes::Set;", "x")),
        ],
    );
    let via = store
        .execute_query(
            "MATCH (cs:CallSite) WHERE cs.callee_name = 'x.answer' RETURN cs.receiver_hint_via",
        )
        .expect("via")
        .rows;
    assert_eq!(
        via,
        vec![vec!["return-type-import:mine".to_string()]],
        "the mark stays as written"
    );
    resolver::resolve_graph(&store).expect("resolve");
    assert_eq!(targets(&store, "x"), vec!["src/shapes.rs::Set::answer"]);
    let recorded = store.crate_evidence();
    let mut renamed = recorded.clone();
    renamed.crate_names = std::collections::BTreeSet::from(["other".to_string()]);
    store
        .write_crate_evidence(&renamed)
        .expect("record other facts");
    resolver::resolve_graph(&store).expect("resolve under the new facts");
    assert!(
        targets(&store, "x").is_empty(),
        "an earlier run's row must not survive"
    );
    store
        .write_crate_evidence(&recorded)
        .expect("record the facts again");
    resolver::resolve_graph(&store).expect("resolve again");
    assert_eq!(targets(&store, "x"), vec!["src/shapes.rs::Set::answer"]);
}
