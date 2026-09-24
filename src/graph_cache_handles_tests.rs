// graph_cache_handles_tests — issue #352: a handle the read cache keeps open
// must never outlive a write to the same graph, because closing it writes its
// stale view over what the writer committed.
//
// Each test seeds a graph with one Function node, keeps the read cache's handle
// open, lets a second handle (what a write tool opens) add a node, and then
// reads the graph back through a fresh handle. The added node must be there.

use super::{open_cached, CACHE};
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
