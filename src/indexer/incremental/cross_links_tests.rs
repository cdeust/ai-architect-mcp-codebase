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
