//! Issue #353, review rounds 2 and 3: `CallSite.is_resolved` after a purge.
//!
//! An incremental refresh purges the target with its file and used to leave the
//! flag of the unchanged caller's site as it was (`true`), so the graph claimed a
//! resolution it no longer had. The purge is the source of truth: the sites that
//! had a resolution edge into a purged node are reopened. A site with NO edge but
//! a legitimate resolution (a tuple-struct constructor call, issue #356, has no
//! per-site row; a macro site; a language-server site) keeps its flag.
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

    fn resolve(&self) -> resolver::ResolutionResult {
        resolver::resolve_graph(&self.store()).expect("resolve")
    }

    fn edit(&self, name: &str, text: &str) {
        fs::write(self.source.join(name), text).unwrap();
    }

    fn refresh(&self) {
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

    /// The number of call sites flagged resolved.
    fn resolved_count(&self) -> usize {
        self.store()
            .execute_query("MATCH (c:CallSite) WHERE c.is_resolved = true RETURN c.id")
            .unwrap()
            .rows
            .len()
    }

    fn site_rows(&self, callee: &str) -> usize {
        self.store()
            .execute_query(&format!(
                "MATCH (c:CallSite)-[r]->() WHERE c.callee_name = '{callee}' RETURN label(r)"
            ))
            .unwrap()
            .rows
            .len()
    }

    fn flag(&self, callee: &str) -> String {
        let rows = self
            .store()
            .execute_query(&format!(
                "MATCH (c:CallSite) WHERE c.callee_name = '{callee}' RETURN c.is_resolved"
            ))
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        rows[0][0].to_lowercase()
    }
}

/// (a) The exact repro of the second review: `a.rs` is deleted, `b.rs` is
/// unchanged.
#[test]
fn a_site_whose_target_was_purged_is_unresolved_after_the_next_resolve() {
    let f = Fixture::new(&[
        ("a.rs", "pub fn helper() {}\n"),
        ("b.rs", "fn f() {\n    helper();\n}\n"),
    ]);
    assert_eq!(
        f.flag("helper"),
        "true",
        "control: resolved before the purge"
    );
    fs::remove_file(f.source.join("a.rs")).unwrap();
    f.refresh();
    f.resolve();
    assert_eq!(
        f.flag("helper"),
        "false",
        "the graph claims a resolution it lost"
    );
}

/// The same purge by REWRITE: the target file is rewritten without the target.
#[test]
fn a_site_whose_target_was_rewritten_away_is_unresolved() {
    let f = Fixture::new(&[
        ("a.rs", "pub fn helper() {}\n"),
        ("b.rs", "fn f() {\n    helper();\n}\n"),
    ]);
    f.edit("a.rs", "pub fn other() {}\n");
    f.refresh();
    f.resolve();
    assert_eq!(f.flag("helper"), "false");
}

/// A rewrite that keeps the target keeps the resolution: the edge is relinked.
#[test]
fn a_site_whose_target_survives_a_rewrite_stays_resolved() {
    let f = Fixture::new(&[
        ("a.rs", "pub fn helper() {}\n"),
        ("b.rs", "fn f() {\n    helper();\n}\n"),
    ]);
    f.edit("a.rs", "pub fn helper() {}\npub fn extra() {}\n");
    f.refresh();
    f.resolve();
    assert_eq!(f.flag("helper"), "true");
    assert!(f.site_rows("helper") > 0, "the per-site row was lost");
}

const SHAPES_A: &str = "pub fn helper() {}\npub struct P(pub u32);\n";
const SHAPES_B: &str = "fn f() {\n    helper();\n    let _p = P(1);\n    println!(\"x\");\n}\n";

/// (b) A call to a tuple-struct constructor is flagged resolved and holds a
/// per-site row (`Calls_CallSite_Struct`, issue #356). Its file did not change:
/// it keeps the flag and the row through an incremental refresh and a resolve.
#[test]
fn a_constructor_call_keeps_its_row_and_its_flag_through_an_unrelated_refresh() {
    let f = Fixture::new(&[
        ("a.rs", SHAPES_A),
        ("b.rs", SHAPES_B),
        ("c.rs", "fn unrelated() {}\n"),
    ]);
    assert_eq!(f.flag("P"), "true", "control: the constructor is resolved");
    assert_eq!(f.site_rows("P"), 1, "control: and has its per-site row");
    f.edit("c.rs", "fn unrelated() {}\nfn touched() {}\n");
    f.refresh();
    f.resolve();
    assert_eq!(f.flag("P"), "true", "a valid resolution was flipped");
    assert_eq!(f.site_rows("P"), 1, "the per-site row was lost");
}

/// (d) A macro site resolved by the macro tier keeps its state.
#[test]
fn a_macro_resolved_site_keeps_its_state() {
    let f = Fixture::new(&[
        ("a.rs", SHAPES_A),
        ("b.rs", SHAPES_B),
        ("c.rs", "fn unrelated() {}\n"),
    ]);
    let before = (f.flag("println!"), f.site_rows("println!"));
    assert_eq!(before.0, "true", "control: the macro site is resolved");
    f.edit("c.rs", "fn unrelated() {}\nfn touched() {}\n");
    f.refresh();
    f.resolve();
    assert_eq!((f.flag("println!"), f.site_rows("println!")), before);
}

/// (e) Whole fixture: every shape at once, an unrelated file touched, an
/// incremental refresh and a resolve: the count of resolved sites is identical.
#[test]
fn touching_an_unrelated_file_leaves_the_resolved_count_identical() {
    let f = Fixture::new(&[
        ("a.rs", SHAPES_A),
        ("b.rs", SHAPES_B),
        ("c.rs", "fn unrelated() {}\n"),
    ]);
    let before = f.resolved_count();
    assert!(before >= 3, "control: {before} resolved sites");
    f.edit("c.rs", "fn unrelated() {}\nfn touched() {}\n");
    f.refresh();
    f.resolve();
    assert_eq!(f.resolved_count(), before);
}

/// (b2) A site another tier flagged resolved with no per-site row, that the
/// static resolver still cannot resolve (the language server pointed it at a
/// target of a kind no per-site table exists for, such as a Trait). Its file
/// did not change: a resolve that infers "no edge, so no longer resolved" would
/// flip it; the purge is the source of truth, and nothing was purged.
#[test]
fn a_site_flagged_by_another_tier_without_a_row_keeps_its_flag() {
    let f = Fixture::new(&[
        ("a.rs", "pub struct Wrapper(pub u32);\n"),
        ("b.rs", "fn f() {\n    alias(1);\n}\n"),
        ("c.rs", "fn unrelated() {}\n"),
    ]);
    assert_eq!(
        f.flag("alias"),
        "false",
        "control: the static tier finds nothing"
    );
    f.store()
        .execute_query("MATCH (c:CallSite) WHERE c.callee_name = 'alias' SET c.is_resolved = true")
        .unwrap();
    assert_eq!(
        f.site_rows("alias"),
        0,
        "control: and there is no per-site row"
    );
    f.edit("c.rs", "fn unrelated() {}\nfn touched() {}\n");
    f.refresh();
    f.resolve();
    assert_eq!(
        f.flag("alias"),
        "true",
        "a resolution by another tier was flipped"
    );
}
