// graph_cache_handles_tests — issue #352: a handle the read cache keeps open
// must never outlive a write to the same graph, because closing it writes its
// stale view over what the writer committed.
//
// Each test seeds a graph with one Function node, keeps the read cache's handle
// open, lets a second handle (what a write tool opens) add a node, and then
// reads the graph back through a fresh handle. The added node must be there.

use super::{open_cached, release, CACHE};
use crate::graph_store::GraphStore;
use std::path::Path;

fn seed(path: &Path) {
    let store = GraphStore::open_or_create(path).expect("open");
    store.create_schema().expect("schema");
    store
        .execute_query("CREATE (:Function {id: 'base', name: 'base'})")
        .expect("seed");
}

fn functions(path: &Path) -> String {
    let store = GraphStore::open_or_create(path).expect("open for count");
    store
        .execute_query("MATCH (n:Function) RETURN count(n)")
        .expect("count")
        .rows[0][0]
        .clone()
}

fn write_one(path: &Path, id: &str) {
    let store = GraphStore::open_or_create(path).expect("open writer");
    store
        .execute_query(&format!("CREATE (:Function {{id: '{id}', name: '{id}'}})"))
        .expect("write");
}

fn read_through_cache(path: &Path) -> String {
    let store = open_cached(path).expect("cached open");
    store
        .execute_query("MATCH (n:Function) RETURN count(n)")
        .expect("cached count")
        .rows[0][0]
        .clone()
}

#[test]
fn a_row_committed_after_a_cached_read_survives_the_next_cached_read() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    assert_eq!(read_through_cache(&path), "1");

    write_one(&path, "written_after_the_read");
    assert_eq!(
        read_through_cache(&path),
        "2",
        "the cache must see the write"
    );
    assert_eq!(functions(&path), "2", "the write must be durable");
}

#[test]
fn a_row_committed_after_a_cached_read_survives_the_end_of_the_process_state() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    assert_eq!(read_through_cache(&path), "1");

    write_one(&path, "written_after_the_read");
    // Nothing reads through the cache again: the handle it kept is released
    // when the process ends, and must not undo the write.
    CACHE.with(|c| c.borrow_mut().entries.clear());
    assert_eq!(functions(&path), "2");
}

#[test]
fn several_writes_between_cached_reads_all_survive() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    assert_eq!(read_through_cache(&path), "1");
    write_one(&path, "w1");
    assert_eq!(read_through_cache(&path), "2");
    write_one(&path, "w2");
    write_one(&path, "w3");
    assert_eq!(read_through_cache(&path), "4");
    assert_eq!(functions(&path), "4");
}

#[test]
fn a_write_before_any_cached_read_is_unaffected() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    write_one(&path, "w1");
    assert_eq!(read_through_cache(&path), "2");
    assert_eq!(functions(&path), "2");
}

fn cached_entries() -> usize {
    CACHE.with(|c| c.borrow().entries.len())
}

fn fresh_cache() {
    CACHE.with(|c| c.borrow_mut().entries.clear());
}

// A request that still holds the handle it got from `open_cached` keeps it
// open past any release, so a write through another handle would be overwritten
// when it closes. The write is refused instead, naming the graph.
#[test]
fn a_write_is_refused_while_a_request_still_holds_the_cached_handle() {
    fresh_cache();
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    let held = open_cached(&path).expect("cached open");

    let refused = match GraphStore::open_or_create(&path) {
        Err(e) => e,
        Ok(_) => panic!("must refuse"),
    };
    assert!(refused.contains("graph_handle_in_use"), "{refused}");
    assert!(refused.contains(&path.display().to_string()), "{refused}");
    assert_eq!(cached_entries(), 1, "the held handle must stay cached");

    drop(held);
    write_one(&path, "after_the_reader_returned");
    assert_eq!(functions(&path), "2");
}

#[test]
fn removing_a_stale_graph_is_refused_while_a_request_holds_the_handle() {
    fresh_cache();
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    let held = open_cached(&path).expect("cached open");
    let refused = match crate::query_handlers::remove_stale_graph_artifact(&path) {
        Err(e) => e,
        Ok(_) => panic!("must refuse"),
    };
    assert!(refused.contains("graph_handle_in_use"), "{refused}");
    assert!(path.exists(), "nothing may be removed");
    drop(held);
}

// The only way the cache is borrowed when a release runs is a re-entrant call
// from inside the cache's own open; it must not read as "nothing to release".
#[test]
fn a_release_while_the_cache_is_borrowed_is_an_error() {
    fresh_cache();
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    let outcome = CACHE.with(|c| {
        let _borrowed = c.borrow_mut();
        release(&path)
    });
    let err = outcome.expect_err("a busy cache must be an error");
    assert!(err.contains("graph_cache_busy"), "{err}");
}

// Every spelling of the same graph reaches the same cached handle.
fn released_through(spelling: impl FnOnce(&std::path::Path) -> std::path::PathBuf) {
    fresh_cache();
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("g");
    seed(&path);
    drop(open_cached(&path).expect("cached open"));
    assert_eq!(cached_entries(), 1);
    release(&spelling(&path)).expect("release");
    assert_eq!(
        cached_entries(),
        0,
        "the alias did not reach the cached handle"
    );
}

#[test]
fn an_absolute_path_releases_the_cached_handle() {
    released_through(|p| p.to_path_buf());
}

#[test]
fn a_symlink_to_the_graph_releases_the_cached_handle() {
    released_through(|p| {
        let link = p.parent().expect("parent").join("link");
        std::os::unix::fs::symlink(p, &link).expect("symlink");
        link
    });
}

#[test]
fn a_relative_path_releases_the_cached_handle() {
    released_through(|p| {
        let cwd = std::env::current_dir().expect("cwd");
        let mut up = std::path::PathBuf::new();
        for _ in cwd.components().skip(1) {
            up.push("..");
        }
        up.join(p.strip_prefix("/").expect("absolute"))
    });
}

#[test]
fn a_trailing_slash_releases_the_cached_handle() {
    released_through(|p| {
        let mut with_slash = p.as_os_str().to_os_string();
        with_slash.push("/");
        std::path::PathBuf::from(with_slash)
    });
}
