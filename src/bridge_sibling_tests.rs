// bridge_sibling_tests: a sibling graph the bridge cannot open is skipped with a
// trace (issue #352), not silently.

use super::*;

fn refuse(path: &Path) -> Result<(), String> {
    Err(format!(
        "graph_handle_in_use: {} is held by a running request",
        path.display()
    ))
}

// The hook is per thread and each test runs on its own thread, so registering
// a refusing one here leaves every other test alone.
#[test]
fn a_sibling_refused_as_held_is_skipped() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    GraphStore::open_or_create(&path)
        .expect("create")
        .create_schema()
        .expect("schema");
    assert!(open_sibling(&path).is_some(), "an unheld sibling opens");
    graph_store::register_release_hook(refuse);
    assert!(open_sibling(&path).is_none(), "a held sibling is skipped");
}
