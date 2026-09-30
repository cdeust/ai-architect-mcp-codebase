// corpora_tests.rs — unit tests for corpora.rs.
//
// Split out of corpora.rs (issue #428: the shallow-checkout skip pushed
// corpora.rs to the 500-line file cap in coding-standards.md §4.1) via
// #[path] inclusion, the same way runner.rs includes runner_tests.rs.

use super::*;
use serde_json::json;
use std::collections::BTreeMap;

fn label(input: Value, expected: Value) -> GroundTruthLabel {
    GroundTruthLabel {
        query_id: "q".into(),
        input,
        expected,
    }
}

#[test]
fn stale_guard_flags_a_deleted_qualified_name_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("parser/mod.rs").parent().unwrap()).unwrap();
    std::fs::write(root.join("parser/mod.rs"), "").unwrap();

    let labels = vec![
        // present: parser/mod.rs exists
        label(
            json!({}),
            json!({ "qualified_name": "parser/mod.rs::parse_file" }),
        ),
        // stale: parser/rust/mod.rs was deleted in the migration
        label(
            json!({}),
            json!({ "qualified_name": "parser/rust/mod.rs::parse_rust_file" }),
        ),
    ];
    let stale = stale_ground_truth(root, &labels);
    assert_eq!(stale, vec!["parser/rust/mod.rs".to_string()]);
}

#[test]
fn stale_guard_reads_partition_qn_and_query_literals() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::write(root.join("live.rs"), "").unwrap();

    let labels = vec![
        // q12 partition rows use `qn`
        label(
            json!({}),
            json!({ "partition": [ { "qn": "gone.rs::Foo", "cluster": 1 } ] }),
        ),
        // q9 embeds the file path in a Cypher `f.path = '...'` literal
        label(
            json!({ "query": "MATCH (f:File) WHERE f.path = 'also_gone.rs' RETURN n.path" }),
            json!({ "imports": [] }),
        ),
        // q11 embeds it in `s.qualified_name = '<path>::<name>'`
        label(
            json!({ "query": "MATCH (s:Struct) WHERE s.qualified_name = 'live.rs::Bar' RETURN f.name" }),
            json!({ "fields": [] }),
        ),
    ];
    let stale = stale_ground_truth(root, &labels);
    // Both deleted paths flagged; the live one is not.
    assert_eq!(
        stale,
        vec!["also_gone.rs".to_string(), "gone.rs".to_string()]
    );
}

#[test]
fn stale_guard_query_extraction_is_position_and_type_accurate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::write(root.join("live.rs"), "").unwrap();

    let labels = vec![
        // A `.rs` literal BEFORE the marker (`decoy.rs`) must NOT be read —
        // only tokens that FOLLOW a marker count. Two `f.path` markers, one
        // deleted (`gone_a.rs`) and one live (`live.rs`).
        label(
            json!({ "query": "WHERE x.name = 'decoy.rs' AND f.path = 'gone_a.rs' AND f.path = 'live.rs' RETURN n" }),
            json!({ "imports": [] }),
        ),
        // `s.qualified_name` literal: the path is the part before `::`.
        label(
            json!({ "query": "WHERE s.qualified_name = 'gone_b.rs::Foo' RETURN f" }),
            json!({ "fields": [] }),
        ),
    ];
    let stale = stale_ground_truth(root, &labels);
    // decoy.rs excluded (pre-marker); live.rs excluded (exists); the two
    // deleted paths flagged, `::Foo` stripped from the qualified name.
    assert_eq!(
        stale,
        vec!["gone_a.rs".to_string(), "gone_b.rs".to_string()]
    );
}

/// Issue #428: every `(corpus dir, name, git_rev)` whose manifest sets
/// `git_rev` — the input `pinned::shallow_skips` filters.
fn git_rev_pins(corpora_root: &Path) -> Vec<(PathBuf, String, String)> {
    let Ok(entries) = fs::read_dir(corpora_root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let manifest = read_manifest(&dir.join("corpus.toml")).ok()?;
            let rev = manifest.git_rev.clone()?;
            Some((dir, manifest.name, rev))
        })
        .collect()
}

/// Issue #428: corpus directory name -> skip reason, for each pinned corpus
/// whose rev this shallow checkout dropped. Only those corpora are skipped;
/// every other corpus is still checked. Each reason is printed uncaptured,
/// so every skipped corpus is named in the test output.
fn shallow_skipped(corpora_root: &Path) -> BTreeMap<String, String> {
    pinned::shallow_skips(&git_rev_pins(corpora_root))
        .into_iter()
        .map(|(dir, reason)| {
            pinned::eprint_uncaptured(&format!("[bench] skipping: {reason}"));
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
            (name.unwrap_or_default(), reason)
        })
        .collect()
}

/// `discover_except`'s postcondition: a skipped corpus is never loaded, so
/// its error cannot fail the discovery of the others.
#[test]
fn a_skipped_corpus_is_never_loaded_and_the_others_still_are() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let broken = tmp.path().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("corpus.toml"), "not = [valid").unwrap();
    assert!(discover_all(tmp.path()).is_err());
    let loaded = discover_except(tmp.path(), |name| name == "broken").expect("skip broken");
    assert!(loaded.is_empty());
}

/// Issue #359: the guard above only warned when the whole benchmark ran,
/// and three module splits (#132, #210, #359) left dead labels behind. This
/// test reads the real corpora, so a split that deletes a labelled path
/// fails `cargo test` instead of silently scoring zero.
#[test]
fn every_label_of_every_corpus_references_existing_paths() {
    let corpora_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpora");
    let skipped = shallow_skipped(&corpora_root);
    let corpora = discover_except(&corpora_root, |name| skipped.contains_key(name))
        .expect("load benches/corpora");
    assert!(!corpora.is_empty(), "no labelled corpus found");
    let mut dead = Vec::new();
    for corpus in &corpora {
        for rel in stale_ground_truth(&corpus.source_path, &corpus.labels) {
            dead.push(format!("{}: source path {rel}", corpus.name));
        }
        for label in &corpus.labels {
            for key in FIXTURE_PATH_KEYS {
                let Some(rel) = label.input.get(*key).and_then(Value::as_str) else {
                    continue;
                };
                if Path::new(rel).is_absolute() || !corpus.corpus_dir.join(rel).exists() {
                    dead.push(format!("{}: {key} {rel}", corpus.name));
                }
            }
        }
    }
    assert!(dead.is_empty(), "labels reference deleted paths: {dead:?}");
}

#[test]
fn stale_guard_flags_an_absolute_path_even_when_it_exists() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let abs = root.join("here.rs");
    std::fs::write(&abs, "").unwrap();
    let abs = abs.to_str().expect("utf-8 path").to_string();
    let labels = vec![label(
        json!({ "query": format!("WHERE f.path = '{abs}' RETURN n.path") }),
        json!({ "imports": [] }),
    )];
    assert_eq!(stale_ground_truth(root, &labels), vec![abs]);
}

#[test]
fn stale_guard_reads_non_rust_source_paths_in_queries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::write(root.join("app.ts"), "").unwrap();
    let labels = vec![label(
        json!({ "query": "WHERE f.path = 'app.ts' OR f.path = 'gone.ts' RETURN n.path" }),
        json!({ "imports": [] }),
    )];
    assert_eq!(
        stale_ground_truth(root, &labels),
        vec!["gone.ts".to_string()]
    );
}

#[test]
fn stale_guard_is_empty_when_every_path_exists() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::write(root.join("a.rs"), "").unwrap();
    let labels = vec![label(
        json!({ "qualified_name": "a.rs::A" }),
        json!({ "qualified_name": "a.rs::A" }),
    )];
    assert!(stale_ground_truth(root, &labels).is_empty());
}

/// Issue #397: `rust-self` must read its pinned commit, never the live
/// `src/`, or its exhaustive labels rot with every merged PR.
#[test]
fn the_rust_self_corpus_is_read_from_its_pinned_revision() {
    let corpora_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpora");
    if shallow_skipped(&corpora_root).contains_key("rust-self") {
        return;
    }
    let corpus = load_one(&corpora_root, "rust-self").expect("load rust-self");
    assert!(
        corpus._pinned_tree.is_some(),
        "rust-self lost its git_rev pin"
    );
    let live_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    assert_ne!(
        corpus.source_path.canonicalize().unwrap(),
        live_src.canonicalize().unwrap(),
        "the corpus points at the moving working tree"
    );
}
