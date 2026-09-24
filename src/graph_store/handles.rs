// graph_store::handles: one live handle per graph inside the process (issue #352).
//
// What backs the rule, exactly:
//   - lbug 0.20.4 `Database::~Database` (lbug-src/src/main/database.cpp:147-153)
//     runs a checkpoint of THAT instance's in-memory state when it closes,
//     unless it was opened read-only or with forceCheckpointOnClose off. The
//     Rust `SystemConfig` exposes `read_only` and no forceCheckpointOnClose.
//   - Measured on this crate (graph_cache::handles_tests): with two handles on
//     one graph in one process, a row committed through the second one is
//     invisible to the first (it still counts 1), and after the first is
//     dropped a fresh open counts 1, not 2. End to end, `lsp_resolve` after a
//     `query_graph` left 0 durable LSP rows against 1 without the query.
//   The source shows that a close writes the closing instance's own view; the
//   measurement shows the effect. The exact page-level overwrite is not traced.
//
// The read path keeps a handle open between requests (`graph_cache`). A write
// tool opens its own handle to the same graph, so without a rule the cached
// handle outlives the write and undoes it, silently.
//
// The rule: before a handle is opened, a graph rewritten or a graph removed,
// the handles the read cache holds for that graph are released. If a request
// still holds one (its `Rc` is alive) the release cannot make the close safe,
// so it fails and the open is refused with an error naming the graph: a
// refused write is loud, a stale close is silent. The cache registers itself
// here, so this layer needs no knowledge of it.

use std::cell::Cell;
use std::path::Path;

/// Drops the cached handles of one graph, or says why it cannot.
pub type ReleaseHook = fn(&Path) -> Result<(), String>;

thread_local! {
    static RELEASE_HOOK: Cell<Option<ReleaseHook>> = const { Cell::new(None) };
}

/// Registers the function that drops the cached handles of one graph. The
/// server is single-threaded, so the hook is per thread like the cache it
/// serves (see `graph_cache`). Registering twice keeps the last hook.
pub fn register_release_hook(hook: ReleaseHook) {
    RELEASE_HOOK.with(|h| h.set(Some(hook)));
}

/// Releases every cached read handle for the graph at `path`; an error when a
/// running request still holds one or the cache is busy. A no-op when no cache
/// was ever used on this thread.
pub fn release_open_handles(path: &Path) -> Result<(), String> {
    match RELEASE_HOOK.with(Cell::get) {
        Some(hook) => hook(path),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static SEEN: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    fn record(path: &Path) -> Result<(), String> {
        SEEN.with(|s| s.borrow_mut().push(path.to_path_buf()));
        Ok(())
    }

    fn refuse(path: &Path) -> Result<(), String> {
        Err(format!("refused {}", path.display()))
    }

    #[test]
    fn release_is_a_noop_until_a_hook_is_registered_then_reaches_it() {
        std::thread::spawn(|| {
            assert_eq!(release_open_handles(Path::new("/nowhere")), Ok(()));
            assert!(SEEN.with(|s| s.borrow().is_empty()));
            register_release_hook(record);
            assert_eq!(release_open_handles(Path::new("/some/graph")), Ok(()));
            assert_eq!(
                SEEN.with(|s| s.borrow().clone()),
                vec![PathBuf::from("/some/graph")]
            );
        })
        .join()
        .expect("thread");
    }

    #[test]
    fn a_refusal_from_the_hook_reaches_the_caller() {
        std::thread::spawn(|| {
            register_release_hook(refuse);
            assert_eq!(
                release_open_handles(Path::new("/g")),
                Err("refused /g".to_string())
            );
        })
        .join()
        .expect("thread");
    }
}
