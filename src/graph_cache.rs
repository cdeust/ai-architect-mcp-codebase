//! Process-local cache of opened `GraphStore` handles for the read path.
//!
//! # Why this exists (Phase 3, bounded-io plan)
//!
//! Every read-only MCP tool (`query_graph`, `get_symbol`, `get_impact`,
//! `get_processes`, `search_codebase`, `get_context`, `cluster_graph`'s read
//! probes, the PRD/security/diff validators) opens the backing graph with
//! `GraphStore::open_or_create(path)` and then throws the handle away when the
//! request returns. The handle is an **embedded LadybugDB database** (`lbug`
//! 0.15) opened over FFI — `Database::new` + `Connection::new` pay a real
//! C++-side open/recover cost on every call. Opening once and reusing the
//! handle across requests to the same graph removes that per-request cost.
//!
//! # Architectural facts this design is built on (verified, not assumed)
//!
//! 1. **The MCP server is strictly single-threaded and sequential.** `main()`
//!    reads stdin line by line (`stdin.lock().lines()`) and runs each request to
//!    completion before reading the next. The one thread in `src/` is the LSP
//!    frame reader (`lsp_client/frames.rs`), which never touches a graph, and
//!    there is no `tokio` or `rayon`; `tests/single_thread_graph_handles.rs`
//!    enforces both. Therefore two requests can NEVER
//!    miss the cache simultaneously — "single-flight the concurrent load" is
//!    satisfied for free by the runtime, and exactly one open happens per
//!    (path, generation). A `thread_local!` cache is the correct primitive: it
//!    is reused across every request on the one request-processing thread.
//!
//! 2. **`GraphStore` is `!Sync`** — it holds a `RefCell<HashMap<…>>` statement
//!    cache. A process-wide `static OnceLock<Arc<RwLock<…>>>` over a `!Sync`
//!    value is impossible without `unsafe`, and there is no `Send`/`Sync` need
//!    to justify that unsafety in a single-threaded server. `thread_local!` +
//!    `Rc` matches the data's real thread-affinity exactly: the handle never
//!    leaves the thread that opened it.
//!
//! 3. **Write tools delete and rewrite the graph dir.** `index_codebase`
//!    `remove_dir_all`s the graph directory and rebuilds it; `resolve_graph`,
//!    `cluster_graph`, and `index_history` mutate it in place. A cached open
//!    handle to a deleted/rewritten directory is a correctness bug (stale reads,
//!    and on an embedded DB a held directory lock can even block the rewrite).
//!    Hence: (a) write tools NEVER use this cache — they keep calling
//!    `open_or_create` directly for a fresh exclusive handle; (b) the read cache
//!    revalidates staleness on every lookup and drops the handle the instant the
//!    on-disk graph changed.
//!
//! # Single-flight + invalidation, in three sentences
//!
//! On lookup we snapshot the graph's on-disk fingerprint (newest mtime + total
//! byte size across the DB path tree) and compare it to the fingerprint stored
//! beside the cached handle; if they match we hand back the same `Rc<GraphStore>`
//! (pointer-identical — no re-open), and if they differ (or the entry is absent)
//! we open exactly once, store the handle with its fresh fingerprint, and return
//! it. Because the server is single-threaded the open is inherently
//! single-flight — no two lookups are ever in flight at once, so the file is
//! deserialized/opened at most once per on-disk generation. Staleness is decided
//! by the fingerprint, never by a TTL, so a graph rewritten by the indexer is
//! reloaded on its very next read and a never-changing graph is never reopened.

use crate::graph_store::GraphStore;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Cache capacity.
//
// source: deployment topology — one indexed repository produces exactly one
// graph path (`<output_dir>/graph`), and a single Claude Code session drives one
// repo's pipeline at a time. The realistic distinct-key count is 1; a small
// constant of a few entries covers a user who switches between a handful of
// repos within one server process without bounding growth so tightly that a
// legitimate working set thrashes. The key space is operator-controlled (a
// caller can only name graph paths that exist on disk and pass the
// `validate_graph_path_safe` gate), so this is a soft working-set bound, not a
// security limit. On overflow we evict the least-recently-opened entry, which
// for the dominant single-graph workload never triggers.
//
// 8 is derived as "small multiple of the expected 1 distinct graph that still
// absorbs a multi-repo session" — not a tuned performance constant. Changing it
// has no correctness effect; staleness revalidation is per-lookup regardless.
const MAX_CACHED_GRAPHS: usize = 8; // source: deployment topology (1 graph/repo); soft working-set bound

// ---------------------------------------------------------------------------
// Fingerprint — the staleness signal.
// ---------------------------------------------------------------------------

/// On-disk fingerprint of a graph: the newest modification time and the total
/// byte size across every file in the database path tree. An embedded LadybugDB
/// stores its data in files inside the path; any write (the indexer rebuilding
/// the dir, the resolver adding edges) changes at least one inner file's mtime
/// or the aggregate size. Comparing both fields catches an in-place edit that
/// preserves size as well as a rewrite that preserves a single file's mtime.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct GraphFingerprint {
    newest_mtime_nanos: u128,
    total_size_bytes: u64,
}

impl GraphFingerprint {
    /// Computes the fingerprint by walking the DB path. Returns `None` if the
    /// path does not exist or cannot be stat-ed — callers treat `None` as "no
    /// stable fingerprint", which forces a miss (re-open) rather than serving a
    /// possibly-stale handle.
    fn of(path: &Path) -> Option<Self> {
        let mut newest_mtime_nanos: u128 = 0;
        let mut total_size_bytes: u64 = 0;
        accumulate(path, &mut newest_mtime_nanos, &mut total_size_bytes).ok()?;
        Some(GraphFingerprint {
            newest_mtime_nanos,
            total_size_bytes,
        })
    }
}

/// Recursively folds the newest mtime and total size over `path`. A file
/// contributes its own metadata; a directory contributes the metadata of every
/// descendant. Symlinks are followed via `metadata` (not `symlink_metadata`)
/// because the graph dir is process-created and not expected to contain
/// adversarial symlinks; the upstream `validate_graph_path_safe` gate already
/// rejects unsafe graph paths before any cache lookup.
fn accumulate(path: &Path, newest: &mut u128, total: &mut u64) -> std::io::Result<()> {
    let meta = std::fs::metadata(path)?;
    if let Ok(modified) = meta.modified() {
        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
            let nanos = dur.as_nanos();
            if nanos > *newest {
                *newest = nanos;
            }
        }
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            accumulate(&entry.path(), newest, total)?;
        }
    } else {
        *total = total.saturating_add(meta.len());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cache entry + thread-local store.
// ---------------------------------------------------------------------------

struct CacheEntry {
    store: Rc<GraphStore>,
    fingerprint: GraphFingerprint,
    /// Monotonic open sequence number; the smallest one is the LRU eviction
    /// victim when the cache is at capacity.
    opened_seq: u64,
}

struct GraphCache {
    entries: HashMap<PathBuf, CacheEntry>,
    next_seq: u64,
    /// Total number of `GraphStore::open_or_create` calls this cache has issued.
    /// Used by tests to prove single-flight (concurrent-miss → one open) and
    /// reload-on-staleness. Behind `cfg(test)` so it costs nothing in release.
    #[cfg(test)]
    opens: u64,
}

impl GraphCache {
    fn new() -> Self {
        GraphCache {
            entries: HashMap::new(),
            next_seq: 0,
            #[cfg(test)]
            opens: 0,
        }
    }

    /// Returns a cached handle for `path` if present AND its on-disk
    /// fingerprint still matches; otherwise opens the graph, caches the handle
    /// with its current fingerprint, and returns it. Exactly one
    /// `open_or_create` runs per (path, on-disk generation).
    fn get(&mut self, path: &Path) -> Result<Rc<GraphStore>, String> {
        self.get_with_opener(path, GraphStore::open_for_cache)
    }

    // `get_with_config`, issue #25's test-only seam for opening the cache at
    // an explicit `max_db_size`, was removed with the 8 GiB-cap repeal
    // (2026-08-14): its sole consumer was the test that opened at the
    // production byte count, a property that no longer holds by design.

    /// Shared bookkeeping for `get`: fingerprint check, cache hit/miss, and
    /// LRU eviction. `opener` decides how a fresh `GraphStore` is built on a
    /// miss, keeping the bookkeeping independent of the open path.
    fn get_with_opener(
        &mut self,
        path: &Path,
        opener: impl FnOnce(&Path) -> Result<GraphStore, String>,
    ) -> Result<Rc<GraphStore>, String> {
        // Canonicalize so two spellings of the same path share one entry. Fall
        // back to the literal path if canonicalization fails (e.g. the path was
        // just removed) — that yields a miss, which is the safe direction.
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        let current = GraphFingerprint::of(path);

        // Cache hit only when we have a fresh fingerprint that matches the
        // stored one. A `None` current fingerprint (path vanished) is never a
        // hit — we drop any stale entry and re-open below.
        if let (Some(current_fp), Some(entry)) = (current, self.entries.get(&key)) {
            if entry.fingerprint == current_fp {
                return Ok(Rc::clone(&entry.store));
            }
        }

        // Miss (absent, stale, or unfingerprintable): open exactly once.
        let store = Rc::new(opener(path)?);
        #[cfg(test)]
        {
            self.opens += 1;
        }

        // Re-fingerprint AFTER opening so the stored fingerprint reflects the
        // state the handle actually sees. If we still cannot fingerprint, store
        // a zero fingerprint, which can never equal a real `of()` result with a
        // non-empty graph, forcing a re-open next time (fail safe).
        let fingerprint = GraphFingerprint::of(path).unwrap_or(GraphFingerprint {
            newest_mtime_nanos: 0,
            total_size_bytes: 0,
        });

        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.insert(
            key,
            CacheEntry {
                store: Rc::clone(&store),
                fingerprint,
                opened_seq: seq,
            },
        );
        self.evict_if_over_capacity();
        Ok(store)
    }

    /// Evicts the least-recently-opened entry while over capacity. The dominant
    /// single-graph workload never reaches capacity, so this is a safety valve
    /// against unbounded growth when a process serves many distinct repos.
    fn evict_if_over_capacity(&mut self) {
        while self.entries.len() > MAX_CACHED_GRAPHS {
            if let Some(victim_key) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.opened_seq)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&victim_key);
            } else {
                break;
            }
        }
    }
}

// SINGLE-THREAD INVARIANT (issue #352). The cache and the release hook that
// `graph_store::handles` calls are per thread, so a graph opened on another
// thread would neither see nor release this cache's handle. The server runs
// every request on one thread; `tests/single_thread_graph_handles.rs` fails if
// production code starts a thread outside its allowlist.
thread_local! {
    static CACHE: RefCell<GraphCache> = RefCell::new(GraphCache::new());
}

/// Returns a process-local, reused `GraphStore` for `path`, opening it at most
/// once per on-disk generation. Intended for the **read-only** tool path only;
/// write tools (`index_codebase`, `resolve_graph`, `cluster_graph`,
/// `index_history`) must keep calling `GraphStore::open_or_create` directly to
/// obtain a fresh exclusive handle, because they delete or rewrite the graph
/// directory.
///
/// precondition: `path` points at a graph the caller intends to READ.
/// postcondition: the returned handle reflects the graph's current on-disk
/// state (revalidated by fingerprint on every call); two calls with no
/// intervening on-disk change return pointer-identical `Rc`s.
pub fn open_cached(path: &Path) -> Result<Rc<GraphStore>, String> {
    crate::graph_store::register_release_hook(release);
    CACHE.with(|c| c.borrow_mut().get(path))
}

/// Drops the cached handle of the graph at `path`, or refuses. Called before
/// anything opens, rewrites or removes that graph (issue #352, see
/// `graph_store::handles`): a handle kept across a write closes on stale pages
/// and undoes it.
///
/// Refuses (an error naming the graph) when a running request still holds the
/// handle (`Rc::strong_count > 1`): dropping the cache's own reference would
/// not close it, so its later close would still overwrite the write. Also
/// refuses when the cache is borrowed at this moment, which only a re-entrant
/// call from inside the cache's own open could cause. `path` is canonicalised
/// like the cache key, so a symlink, a relative path or a trailing slash reach
/// the same entry.
fn release(path: &Path) -> Result<(), String> {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // `try_with` fails only while the thread's storage is being destroyed, when
    // no request can run any more: nothing to release.
    CACHE
        .try_with(|c| {
            let Ok(mut cache) = c.try_borrow_mut() else {
                return Err(format!(
                    "graph_cache_busy: the handle cache was borrowed while releasing {}",
                    path.display()
                ));
            };
            match cache.entries.get(&key) {
                Some(entry) if Rc::strong_count(&entry.store) > 1 => Err(format!(
                    "graph_handle_in_use: {} is held by a running request, and a \
                     write through another handle would be overwritten when it closes",
                    path.display()
                )),
                Some(_) => {
                    cache.entries.remove(&key);
                    Ok(())
                }
                None => Ok(()),
            }
        })
        .unwrap_or(Ok(()))
}

#[cfg(test)]
#[path = "graph_cache_handles_tests.rs"]
mod handles_tests;

#[cfg(test)]
#[path = "graph_cache_tests.rs"]
mod tests;
