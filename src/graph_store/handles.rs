// graph_store::handles: one live handle per graph inside the process (issue #352).
//
// LadybugDB keeps its own view of the pages it read. Two `Database` values on
// the same graph inside one process do not see each other's commits, and the
// one that closes last writes its stale view over the other's. Measured: a row
// committed through a second handle is gone once the first, older handle is
// dropped, and a read through the older handle still answers with the old
// count.
//
// The read path keeps a handle open between requests (`graph_cache`). A write
// tool opens its own handle to the same graph, so without a rule the cached
// handle outlives the write and undoes it, silently: the rows the writer
// committed disappear while flags set by an earlier pass stay true.
//
// The rule: before a handle is opened, a graph rewritten or a graph deleted,
// every handle the read cache holds for that graph is released first. The
// cache registers itself here, so this layer needs no knowledge of it.

use std::cell::Cell;
use std::path::Path;

thread_local! {
    static RELEASE_HOOK: Cell<Option<fn(&Path)>> = const { Cell::new(None) };
}

/// Registers the function that drops the cached handles of one graph. The
/// server is single-threaded, so the hook is per thread like the cache it
/// serves. Registering twice keeps the last hook.
pub fn register_release_hook(hook: fn(&Path)) {
    RELEASE_HOOK.with(|h| h.set(Some(hook)));
}

/// Releases every cached read handle for the graph at `path`. A no-op when no
/// cache was ever used on this thread.
pub fn release_open_handles(path: &Path) {
    if let Some(hook) = RELEASE_HOOK.with(Cell::get) {
        hook(path);
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

    fn record(path: &Path) {
        SEEN.with(|s| s.borrow_mut().push(path.to_path_buf()));
    }

    #[test]
    fn release_is_a_noop_until_a_hook_is_registered_then_reaches_it() {
        std::thread::spawn(|| {
            release_open_handles(Path::new("/nowhere"));
            assert!(SEEN.with(|s| s.borrow().is_empty()));
            register_release_hook(record);
            release_open_handles(Path::new("/some/graph"));
            assert_eq!(
                SEEN.with(|s| s.borrow().clone()),
                vec![PathBuf::from("/some/graph")]
            );
        })
        .join()
        .expect("thread");
    }
}
