// resolver::retract_edges_tests: issue #443, the rows that are not calls.
// After an incremental refresh, `resolve_graph` leaves the `Imports_*`,
// `Uses_*`, `Implements_*` and `Extends_*` rows, and the `is_resolved` flag of
// every Import, that a full index of the same tree would have.

use crate::graph_store::{GraphStore, REL_TABLES};
use crate::{indexer, resolver};
use std::fs;
use std::path::{Path, PathBuf};

struct Repo {
    _tmp: tempfile::TempDir,
    source: PathBuf,
    graph: PathBuf,
    manifest: PathBuf,
}

/// (resolution rows that are not calls as "table from -> to method", Import
/// flags as "id -> is_resolved"), each sorted, for the graph at `graph`.
type State = (Vec<String>, Vec<String>);

fn state(graph: &Path) -> State {
    let store = GraphStore::open_or_create(graph).expect("open");
    let mut rows = Vec::new();
    for &(rel, from, to) in REL_TABLES {
        if !["Imports_", "Uses_", "Implements_", "Extends_"]
            .iter()
            .any(|p| rel.starts_with(p))
        {
            continue;
        }
        let cypher =
            format!("MATCH (a:{from})-[r:{rel}]->(b:{to}) RETURN a.id, b.id, r.resolution_method");
        for row in store.execute_query(&cypher).expect(&cypher).rows {
            rows.push(format!("{rel} {} -> {} {}", row[0], row[1], row[2]));
        }
    }
    rows.sort();
    let mut imports: Vec<String> = store
        .execute_query("MATCH (i:Import) RETURN i.id, i.is_resolved")
        .expect("imports")
        .rows
        .into_iter()
        .map(|r| format!("{} -> {}", r[0], r[1].to_lowercase()))
        .collect();
    imports.sort();
    (rows, imports)
}

fn resolve(graph: &Path) {
    let store = GraphStore::open_or_create(graph).expect("open");
    resolver::resolve_graph(&store).expect("resolve");
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
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
        resolve(&graph);
        Repo {
            _tmp: tmp,
            source,
            graph,
            manifest,
        }
    }

    /// Applies `edits` (`None` deletes the file), refreshes the graph
    /// incrementally and resolves it.
    fn refresh(&self, edits: &[(&str, Option<&str>)]) {
        for (name, text) in edits {
            match text {
                Some(text) => fs::write(self.source.join(name), text).unwrap(),
                None => fs::remove_file(self.source.join(name)).unwrap(),
            }
        }
        let prior = indexer::manifest::load(&self.manifest).expect("manifest");
        indexer::index_incremental(
            &self.source,
            &self.graph,
            &self.manifest,
            &indexer::IndexOptions::default(),
            &prior,
        )
        .expect("incremental refresh");
        resolve(&self.graph);
    }

    /// The state of a from-scratch index and resolve of the current tree.
    fn fresh_state(&self) -> State {
        let graph = self._tmp.path().join("fresh");
        let _ = fs::remove_dir_all(&graph);
        indexer::index_codebase(&self.source, &graph).expect("fresh index");
        resolve(&graph);
        state(&graph)
    }

    /// Asserts the refreshed graph holds what a fresh index holds, and that
    /// the refresh changed the answer (so the fixture exercises something).
    fn assert_matches_fresh(&self, before: &State) {
        let fresh = self.fresh_state();
        assert_ne!(&fresh, before, "the fixture must change the answer");
        assert_eq!(state(&self.graph), fresh);
    }
}

const PY_USER: &str = "from shapes import Base, helper\n\nclass Child(Base):\n    pass\n\ndef run():\n    return helper()\n";
const PY_SHAPES: &str = "class Base:\n    pass\n\ndef helper():\n    return 1\n";

const RS_LIB: &str = "mod model;\nmod user;\n";
const RS_MODEL: &str = "pub struct Widget;\npub trait Render {\n    fn draw(&self);\n}\n";
const RS_USER: &str = "use crate::model::Render;\nuse crate::model::Widget;\n\npub struct Panel {\n    pub inner: Widget,\n}\n\nimpl Render for Panel {\n    fn draw(&self) {}\n}\n\npub fn build() -> Widget {\n    Widget\n}\n";

fn has(state: &State, needle: &str) -> bool {
    state.0.iter().any(|row| row.contains(needle))
}

#[test]
fn control_the_fixtures_produce_every_kind_of_row() {
    let py = Repo::new(&[("user.py", PY_USER), ("shapes.py", PY_SHAPES)]);
    let s = state(&py.graph);
    assert!(
        has(&s, "Imports_File_Struct user.py -> shapes.py::Base"),
        "{s:#?}"
    );
    assert!(
        has(
            &s,
            "Extends_Struct_Struct user.py::Child -> shapes.py::Base"
        ),
        "{s:#?}"
    );
    let rs = Repo::new(&[
        ("lib.rs", RS_LIB),
        ("model.rs", RS_MODEL),
        ("user.rs", RS_USER),
    ]);
    let s = state(&rs.graph);
    assert!(
        has(&s, "Imports_File_Struct user.rs -> model.rs::Widget"),
        "{s:#?}"
    );
    assert!(
        has(
            &s,
            "Implements_Struct_Trait user.rs::Panel -> model.rs::Render"
        ),
        "{s:#?}"
    );
    assert!(
        has(
            &s,
            "Uses_Field_Struct user.rs::Panel::inner -> model.rs::Widget"
        ),
        "{s:#?}"
    );
    assert!(
        has(
            &s,
            "Uses_Function_Struct user.rs::build -> model.rs::Widget"
        ),
        "{s:#?}"
    );
}

#[test]
fn an_edited_file_that_drops_its_import_loses_the_row() {
    let repo = Repo::new(&[
        ("lib.rs", RS_LIB),
        ("model.rs", RS_MODEL),
        ("user.rs", RS_USER),
    ]);
    let before = state(&repo.graph);
    repo.refresh(&[(
        "user.rs",
        Some("use crate::model::Render;\n\npub struct Panel;\n\nimpl Render for Panel {\n    fn draw(&self) {}\n}\n"),
    )]);
    repo.assert_matches_fresh(&before);
    let after = state(&repo.graph);
    assert!(!has(&after, "model.rs::Widget"), "{after:#?}");
    assert!(
        has(&after, "Imports_File_Trait user.rs -> model.rs::Render"),
        "{after:#?}"
    );
    assert!(
        has(
            &after,
            "Implements_Struct_Trait user.rs::Panel -> model.rs::Render"
        ),
        "{after:#?}"
    );
}

#[test]
fn an_edited_file_that_drops_its_base_class_loses_the_rows() {
    let repo = Repo::new(&[("user.py", PY_USER), ("shapes.py", PY_SHAPES)]);
    let before = state(&repo.graph);
    repo.refresh(&[(
        "user.py",
        Some("from shapes import helper\n\nclass Child:\n    pass\n\ndef run():\n    return helper()\n"),
    )]);
    repo.assert_matches_fresh(&before);
    let after = state(&repo.graph);
    assert!(!has(&after, "shapes.py::Base"), "{after:#?}");
    assert!(has(&after, "user.py -> shapes.py::helper"), "{after:#?}");
}

#[test]
fn an_unchanged_file_loses_the_rows_into_a_symbol_an_edit_removed() {
    let repo = Repo::new(&[
        ("lib.rs", RS_LIB),
        ("model.rs", RS_MODEL),
        ("user.rs", RS_USER),
    ]);
    let before = state(&repo.graph);
    repo.refresh(&[(
        "model.rs",
        Some("pub trait Render {\n    fn draw(&self);\n}\n"),
    )]);
    repo.assert_matches_fresh(&before);
    let after = state(&repo.graph);
    assert!(!has(&after, "model.rs::Widget"), "{after:#?}");
    assert!(
        has(
            &after,
            "Implements_Struct_Trait user.rs::Panel -> model.rs::Render"
        ),
        "{after:#?}"
    );
}

#[test]
fn an_unchanged_file_loses_the_import_row_a_second_definition_makes_ambiguous() {
    // The import twin of `retract_tests`' ambiguous call: `helper` is unique
    // in the first pass, an added file declares a second one. (A base class
    // is left out on purpose: `resolve_extends` takes the first of several
    // candidates in index order, which differs between a refreshed and a
    // fresh graph whatever is retracted.)
    let user = "from shapes import helper\n\ndef run():\n    return helper()\n";
    let repo = Repo::new(&[("user.py", user), ("shapes.py", PY_SHAPES)]);
    let before = state(&repo.graph);
    assert!(has(&before, "user.py -> shapes.py::helper"), "{before:#?}");
    repo.refresh(&[("other.py", Some("def helper():\n    return 2\n"))]);
    repo.assert_matches_fresh(&before);
    let after = state(&repo.graph);
    assert!(!has(&after, "Imports_"), "{after:#?}");
    assert_eq!(after.1, ["user.py::shapes::helper -> false"]);
}

#[test]
fn a_deleted_file_loses_every_row_from_it_and_into_it() {
    let repo = Repo::new(&[
        ("lib.rs", RS_LIB),
        ("model.rs", RS_MODEL),
        ("user.rs", RS_USER),
    ]);
    let before = state(&repo.graph);
    repo.refresh(&[("user.rs", None), ("lib.rs", Some("mod model;\n"))]);
    repo.assert_matches_fresh(&before);
    assert!(!has(&state(&repo.graph), "user.rs"));

    let repo = Repo::new(&[("user.py", PY_USER), ("shapes.py", PY_SHAPES)]);
    let before = state(&repo.graph);
    repo.refresh(&[("shapes.py", None)]);
    repo.assert_matches_fresh(&before);
    assert!(!has(&state(&repo.graph), "shapes.py"));
}

#[test]
fn a_second_resolve_of_an_unchanged_graph_changes_nothing() {
    for files in [
        vec![("user.py", PY_USER), ("shapes.py", PY_SHAPES)],
        vec![
            ("lib.rs", RS_LIB),
            ("model.rs", RS_MODEL),
            ("user.rs", RS_USER),
        ],
    ] {
        let repo = Repo::new(&files);
        let first = state(&repo.graph);
        resolve(&repo.graph);
        assert_eq!(state(&repo.graph), first);
        assert_eq!(first, repo.fresh_state());
    }
}

#[test]
fn a_row_of_another_writer_survives_the_pass() {
    // The light-link pass writes `Imports_File_File` with its own method
    // (`js-regex`); no resolver phase decides it, so no pass may retract it.
    let repo = Repo::new(&[
        ("a.js", "import { x } from './b.js';\nexport const y = x;\n"),
        ("b.js", "export const x = 1;\n"),
    ]);
    let link = "Imports_File_File a.js -> b.js js-regex";
    let first = state(&repo.graph);
    assert!(has(&first, link), "{first:#?}");
    resolve(&repo.graph);
    repo.refresh(&[("c.js", Some("export const z = 2;\n"))]);
    assert!(has(&state(&repo.graph), link));
}

/// The rows of `state` as "table from", one per row, sorted: what each
/// source points at is left out, so two rows of one source show as a repeat.
fn sources(state: &State) -> Vec<String> {
    state
        .0
        .iter()
        .map(|row| row.split(" -> ").next().unwrap_or(row).to_string())
        .collect()
}

#[test]
fn a_reference_decided_again_to_another_target_does_not_keep_both_rows() {
    // Two definitions of each name: the use, implements and extends phases
    // take the first candidate in index order. Editing the file of the one
    // they took moves it behind the other, so the next pass decides the other;
    // the row of the first pass came back with the re-link of the edited file
    // and stayed next to the new one. A fresh index has one row per source.
    let twin = "pub struct Widget;\npub trait Render {\n    fn draw(&self);\n}\n";
    let user = "pub struct Panel {\n    pub inner: Widget,\n}\n\nimpl Render for Panel {\n    fn draw(&self) {}\n}\n";
    let repo = Repo::new(&[("a.rs", twin), ("b.rs", twin), ("user.rs", user)]);
    let before = sources(&state(&repo.graph));
    assert_eq!(
        before,
        [
            "Implements_Struct_Trait user.rs::Panel",
            "Uses_Field_Struct user.rs::Panel::inner"
        ]
    );
    let edited = format!("{twin}pub fn touched() {{}}\n");
    repo.refresh(&[("a.rs", Some(&edited))]);
    assert_eq!(sources(&state(&repo.graph)), before);

    let base = "class Base:\n    pass\n";
    let child = "class Child(Base):\n    pass\n";
    let repo = Repo::new(&[("a.py", base), ("b.py", base), ("user.py", child)]);
    let before = sources(&state(&repo.graph));
    assert_eq!(before, ["Extends_Struct_Struct user.py::Child"]);
    let edited = format!("{base}\ndef touched():\n    return 1\n");
    repo.refresh(&[("a.py", Some(&edited))]);
    assert_eq!(sources(&state(&repo.graph)), before);
}
