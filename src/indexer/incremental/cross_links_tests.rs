//! `cross_links::file_set_changed`: an edit alone leaves the file set as it was.
use super::*;

fn discovered(rel: &str) -> Discovered {
    Discovered {
        rel: rel.to_string(),
        abs: PathBuf::from(rel),
        mtime_ns: 0,
        size: 0,
    }
}

#[test]
fn only_an_add_a_delete_or_a_rename_moves_the_file_set() {
    let edit = ChangeSet {
        changed: vec![discovered("a.js")],
        ..ChangeSet::default()
    };
    assert!(!file_set_changed(&edit));
    let add = ChangeSet {
        added: vec![discovered("b.js")],
        ..ChangeSet::default()
    };
    assert!(file_set_changed(&add));
    let delete = ChangeSet {
        deleted: vec!["b.js".to_string()],
        ..ChangeSet::default()
    };
    assert!(file_set_changed(&delete));
    let rename = ChangeSet {
        renamed: vec![super::super::Rename {
            old_rel: "b.js".to_string(),
            new_file: discovered("c.js"),
        }],
        ..ChangeSet::default()
    };
    assert!(file_set_changed(&rename));
}

/// A tree whose IaC layer carries one gap of each kind on two files each: a
/// FROM-less Dockerfile is a `partial` gap, an oversized Kustomization a
/// `skipped` one. `web/Dockerfile` is clean and draws an `IacResource`.
/// Returns the source dir and the graph store of its full index.
fn iac_gap_tree(tmp: &Path) -> (PathBuf, GraphStore) {
    let src = tmp.join("source");
    for dir in ["a", "b", "c", "d", "web"] {
        std::fs::create_dir_all(src.join(dir)).unwrap();
    }
    std::fs::write(src.join("a/Dockerfile"), "RUN echo a\n").unwrap();
    std::fs::write(src.join("b/Dockerfile"), "RUN echo b\n").unwrap();
    let oversized = "#".repeat(crate::parser::MAX_PARSE_BYTES as usize + 1);
    std::fs::write(src.join("c/kustomization.yaml"), &oversized).unwrap();
    std::fs::write(src.join("d/kustomization.yaml"), &oversized).unwrap();
    std::fs::write(src.join("web/Dockerfile"), "FROM node:18\n").unwrap();
    std::fs::create_dir_all(tmp.join("out")).unwrap();
    let graph = tmp.join("out/graph");
    crate::indexer::index_codebase(&src, &graph).expect("full index");
    let store = GraphStore::open_or_create(&graph).expect("open store");
    (src, store)
}

fn tree_files(src: &Path) -> Vec<PathBuf> {
    [
        "a/Dockerfile",
        "b/Dockerfile",
        "c/kustomization.yaml",
        "d/kustomization.yaml",
        "web/Dockerfile",
    ]
    .iter()
    .map(|rel| src.join(rel))
    .collect()
}

fn gap_rels(gaps: &crate::indexer::iac::IacGaps) -> (Vec<&str>, Vec<&str>) {
    let mut partial: Vec<&str> = gaps.partial.iter().map(|(r, _)| r.as_str()).collect();
    let mut skipped: Vec<&str> = gaps.skipped.iter().map(|(r, _)| r.as_str()).collect();
    partial.sort_unstable();
    skipped.sort_unstable();
    (partial, skipped)
}

/// Issue #443, last slice of the review of #527: `redraw_iac` parses every IaC
/// file of the tree, but returns the gaps of the re-parsed files only. The
/// gaps of an unchanged file are the prior sidecar's, carried forward by
/// `merge_coverage`; the filter keeps the redraw from reporting them as this
/// pass's own. The layer itself is drawn in full: the unchanged clean
/// Dockerfile keeps its resource.
#[test]
fn redraw_iac_reports_the_gaps_of_the_reparsed_files_only() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (src, store) = iac_gap_tree(tmp.path());
    let files = tree_files(&src);

    let reparsed: HashSet<String> = ["a/Dockerfile", "d/kustomization.yaml"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let gaps = redraw_iac(&store, &src, &files, &reparsed).expect("redraw");
    assert_eq!(
        gap_rels(&gaps),
        (vec!["a/Dockerfile"], vec!["d/kustomization.yaml"]),
        "only the re-parsed files' gaps; b/ and c/ are carried forward"
    );
    let drawn = store
        .execute_query(&format!(
            "MATCH (n:{NODE_IAC_RESOURCE}) WHERE n.id = 'web/Dockerfile::dockerfile' RETURN n.id"
        ))
        .expect("query");
    assert_eq!(
        drawn.rows.len(),
        1,
        "the unchanged clean Dockerfile is drawn again"
    );

    let none = redraw_iac(&store, &src, &files, &HashSet::new()).expect("redraw");
    assert_eq!(
        gap_rels(&none),
        (vec![], vec![]),
        "nothing re-parsed, no gap"
    );

    let all: HashSet<String> = files
        .iter()
        .map(|f| {
            f.strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    let every = redraw_iac(&store, &src, &files, &all).expect("redraw");
    assert_eq!(
        gap_rels(&every),
        (
            vec!["a/Dockerfile", "b/Dockerfile"],
            vec!["c/kustomization.yaml", "d/kustomization.yaml"]
        ),
        "every file re-parsed: the gaps a full index records"
    );
}
