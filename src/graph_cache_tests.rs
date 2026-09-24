// graph_cache_tests: the unit tests of `graph_cache` (split out of graph_cache.rs to
// keep that file under the 500-line cap; no test changed).

use super::*;
use crate::graph_store::NODE_FUNCTION;
use std::fs;

/// Resets the thread-local cache so tests do not see each other's entries.
/// Tests in one binary share the thread-local when run on the same thread;
/// each test uses a unique temp path, but we also reset open-counts so the
/// single-flight assertions read clean numbers.
fn with_fresh_cache<F: FnOnce(&mut GraphCache)>(f: F) {
    CACHE.with(|c| {
        *c.borrow_mut() = GraphCache::new();
        f(&mut c.borrow_mut());
    });
}

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("graph_cache_{tag}_{nanos}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn seed_graph(db_path: &Path, fn_id: &str) {
    let store = GraphStore::open_or_create(db_path).expect("open");
    store.create_schema().expect("schema");
    store
        .insert_node(
            NODE_FUNCTION,
            &[
                ("id", &format!("'{fn_id}'")),
                ("name", "'main'"),
                ("qualified_name", "'crate::main'"),
                ("start_line", "1"),
                ("end_line", "10"),
                ("visibility", "'pub'"),
                ("is_async", "false"),
            ],
        )
        .expect("insert");
    // Drop `store` so the embedded DB releases the path before the cache
    // re-opens it (single-writer embedded DBs may hold a directory lock).
    drop(store);
}

#[test]
fn cache_hit_returns_same_arc() {
    let dir = unique_dir("hit");
    let db = dir.join("g");
    seed_graph(&db, "fn1");

    with_fresh_cache(|cache| {
        let a = cache.get(&db).expect("first get");
        let b = cache.get(&db).expect("second get");
        // Pointer identity: the second call reused the deserialized handle.
        assert!(Rc::ptr_eq(&a, &b), "expected same Rc on cache hit");
        // And only one open happened across two gets.
        assert_eq!(cache.opens, 1, "expected exactly one open on a hit");
    });

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn sequential_first_loads_open_once() {
    // The server is single-threaded; "concurrent first-loads" can only ever
    // arrive sequentially. This asserts the single-flight guarantee in the
    // form it can actually occur: N back-to-back lookups of a cold key
    // trigger exactly one open, the rest are hits.
    let dir = unique_dir("single_flight");
    let db = dir.join("g");
    seed_graph(&db, "fn1");

    with_fresh_cache(|cache| {
        let mut handles = Vec::new();
        for _ in 0..16 {
            handles.push(cache.get(&db).expect("get"));
        }
        assert_eq!(cache.opens, 1, "16 lookups must deserialize once");
        // All handles are the same underlying store.
        for h in &handles[1..] {
            assert!(Rc::ptr_eq(&handles[0], h), "all handles share one store");
        }
    });

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fingerprint_change_triggers_reload() {
    let dir = unique_dir("reload");
    let db = dir.join("g");
    seed_graph(&db, "fn1");

    with_fresh_cache(|cache| {
        let first = cache.get(&db).expect("first get");
        assert_eq!(cache.opens, 1);

        // Mutate the on-disk graph: open it fresh, add a node, drop. This
        // changes inner-file mtimes/sizes — the staleness signal.
        {
            let writer = GraphStore::open_or_create(&db).expect("reopen writer");
            writer
                .insert_node(
                    NODE_FUNCTION,
                    &[
                        ("id", "'fn2'"),
                        ("name", "'other'"),
                        ("qualified_name", "'crate::other'"),
                        ("start_line", "20"),
                        ("end_line", "30"),
                        ("visibility", "'pub'"),
                        ("is_async", "false"),
                    ],
                )
                .expect("insert second");
            drop(writer);
        }

        let second = cache.get(&db).expect("second get after change");
        assert_eq!(cache.opens, 2, "on-disk change must force a re-open");
        assert!(
            !Rc::ptr_eq(&first, &second),
            "stale handle must not be reused after the graph changed"
        );
    });

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn evicts_least_recently_opened_over_capacity() {
    // Open MAX_CACHED_GRAPHS + 1 distinct graphs; the first-opened must be
    // evicted, and re-requesting it must re-open (proving it was dropped).
    let parent = unique_dir("evict");
    let mut dbs = Vec::new();
    for i in 0..(MAX_CACHED_GRAPHS + 1) {
        let db = parent.join(format!("g{i}"));
        seed_graph(&db, &format!("fn{i}"));
        dbs.push(db);
    }

    with_fresh_cache(|cache| {
        for db in &dbs {
            cache.get(db).expect("get");
        }
        // One open per distinct graph so far.
        assert_eq!(cache.opens as usize, dbs.len());
        // The oldest (dbs[0]) should have been evicted; re-requesting it
        // re-opens (open count grows by exactly one).
        let opens_before = cache.opens;
        cache.get(&dbs[0]).expect("re-get evicted");
        assert_eq!(
            cache.opens,
            opens_before + 1,
            "evicted entry must re-open on next request"
        );
    });

    let _ = fs::remove_dir_all(&parent);
}

/// Successor to issue #25's regression guard, amended 2026-08-14.
///
/// The original test opened MAX_CACHED_GRAPHS stores at the
/// PRODUCTION byte count to prove the 8 GiB cap prevented VA
/// exhaustion. That cap is repealed (see
/// `DEFAULT_PROD_MAX_DB_SIZE_BYTES`'s doc comment): ingestion must
/// never abort on graph size, so production now runs at lbug's own
/// 8 TiB VM-region ceiling — and opening 8 × 8 TiB reservations
/// inside a parallel `cargo test` run would recreate the EXACT
/// multi-process mmap failure #25 measured. What remains testable
/// here, under the test bound (`AP_LBUG_TEST_MAX_DB_SIZE`, honored
/// by `system_config()`), is the property the cache itself owns:
/// MAX_CACHED_GRAPHS simultaneously-live handles coexist cleanly,
/// plus the cache's bookkeeping at capacity.
///
/// "Concurrently" here means "simultaneously live", not "opened from
/// multiple OS threads": `GraphStore` cannot cross a `thread::spawn`
/// boundary without `unsafe` — its `stmt_cache: RefCell<HashMap<_,
/// PreparedStatement>>` field wraps `lbug::PreparedStatement`, an
/// opaque `cxx::UniquePtr<ffi::PreparedStatement>` that lbug does NOT
/// mark `Send` (contrast with `Database`/`Connection<'_>`, which lbug
/// explicitly marks `unsafe impl Send` — see
/// `lbug-0.15.4/src/database.rs:14-15` and
/// `lbug-0.15.4/src/connection.rs:74-75`). Introducing an unjustified
/// `unsafe impl Send for GraphStore` to force OS-thread concurrency
/// into a test would violate this codebase's local-reasoning
/// discipline for a guarantee this test does not need: the module
/// doc comment above (Architectural fact 1) already establishes the
/// MCP server is single-threaded, so the cache's real concurrency
/// model is "many simultaneously-live handles on one thread", exactly
/// what this test reproduces.
#[test]
fn max_cached_graphs_simultaneously_live_handles_open_cleanly() {
    let parent = unique_dir("prod_bound_simultaneous");
    let dbs: Vec<PathBuf> = (0..MAX_CACHED_GRAPHS)
        .map(|i| parent.join(format!("g{i}")))
        .collect();
    for (i, db) in dbs.iter().enumerate() {
        seed_graph(db, &format!("fn{i}"));
    }

    // Open all MAX_CACHED_GRAPHS graphs under the test bound and
    // hold every handle alive at once (Vec keeps them live for the whole
    // assertion, unlike the cache's LRU which would start evicting at
    // capacity — this test targets VA exhaustion, not eviction policy).
    let mut stores = Vec::with_capacity(dbs.len());
    for (i, db) in dbs.iter().enumerate() {
        let store = GraphStore::open_or_create(db)
            .unwrap_or_else(|e| panic!("graph {i} failed to open under the test bound: {e}"));
        stores.push(store);
    }
    assert_eq!(
        stores.len(),
        MAX_CACHED_GRAPHS,
        "all graphs must open with no VA exhaustion"
    );

    // And the cache's own bookkeeping path (fingerprint + LRU eviction)
    // must still behave correctly when opening at this exact bound —
    // same assertions as evicts_least_recently_opened_over_capacity,
    // above, exercised through the cache's default-config path.
    with_fresh_cache(|cache| {
        for db in &dbs {
            cache.get(db).expect("cache get at the test bound");
        }
        assert_eq!(
            cache.opens as usize,
            dbs.len(),
            "one open per distinct graph, at capacity, no eviction yet"
        );
    });

    drop(stores);
    let _ = fs::remove_dir_all(&parent);
}
