// graph_store — LadybugDB port for the code-intelligence graph.
//
// This module wraps `lbug::Database` + `lbug::Connection` behind a clean
// interface. The rest of the codebase depends only on `GraphStore` methods,
// never on `lbug` directly. If we ever swap the backing store, only this
// file changes.

use lbug::{Connection, Database, LogicalType, PreparedStatement, SystemConfig, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

mod columns;
mod config;
mod ddl;
mod membership;
mod recovery;
mod schema;
mod serialize;
mod writes;
pub use columns::label_declares_column;
use columns::*;
pub use config::*;
use ddl::*;
pub use membership::{community_ids, community_of, process_names, CommunityRow, SymbolMatch};
// Only `tests.rs` reaches these via `use super::*` (production code calls
// `Self::recover_from_stale_sidecars`/`is_stale_sidecar_db_id_error` through
// their full paths) — gated to match, so a non-test build has no unused-import warning.
#[cfg(test)]
use recovery::*;
pub use schema::*;
use serialize::*;

// ---------------------------------------------------------------------------
// Shared edge types — a bulk-insertable graph edge is (from_id, to_id,
// properties). Named here (the module that owns `bulk_insert_edges`) so
// every caller across the crate shares one definition instead of repeating
// the nested-tuple shape (clippy::type_complexity, coding-standards.md §3.1).
// ---------------------------------------------------------------------------

/// A single edge's key-value property list, e.g. `[("weight", "0.4")]`.
pub type EdgeProps = Vec<(String, String)>;

/// One bulk-insertable edge: `(from_id, to_id, properties)`.
pub type PropEdge = (String, String, EdgeProps);

/// A batch of edges of the same relationship table, ready for
/// `GraphStore::bulk_insert_edges`.
pub type PropEdgeList = Vec<PropEdge>;

// ---------------------------------------------------------------------------
// Cypher string escaping — security-critical.
//
// LadybugDB's Cypher dialect (lbug 0.15) exposes no parameterized-query API in
// the Rust crate, so every user-controlled value is interpolated as a string
// literal. An unescaped single quote (or backslash-escape of one) closes the
// literal and allows arbitrary Cypher injection (including `DETACH DELETE`).
//
// Rules (order matters):
//   1. `\\` → `\\\\` (escape backslashes FIRST to avoid double-processing),
//   2. `'`  → `\\'`  (escape quotes after),
//   3. wrap result in single quotes.
//
// source: Neo4j Cypher Manual §"Literals" — string literals use `'` delimiters,
// backslash is the escape character. Applies to LadybugDB which mirrors openCypher.
// ---------------------------------------------------------------------------

/// Escapes a raw string for safe use as a Cypher single-quoted string literal.
/// Returns the string already wrapped in single quotes.
/// ALL user-controlled values heading into Cypher must go through this helper.
pub fn cypher_str(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('\'');
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

// ---------------------------------------------------------------------------
// Bulk-insert batch size.
//
// source: Kuzu/LadybugDB practitioner guidance — batching multiple CREATE
// clauses in a single Cypher query amortizes parser + planner + lock costs.
// 500 is a common sweet spot: large enough to dominate per-call overhead,
// small enough to keep the generated Cypher text under typical statement
// limits. Tunable; not derived from a paper.
// source: empirical — per-row CREATE was measured as the dominant indexing
// cost in the Fermi scalability audit of this codebase (April 2026).
pub const BULK_BATCH_SIZE: usize = 500;

// ---------------------------------------------------------------------------
// QueryResult — thin wrapper returned by execute_query
// ---------------------------------------------------------------------------

/// Result of a Cypher query: column names + row data as strings.
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

// ---------------------------------------------------------------------------
// GraphStore — owns the lbug Database + Connection
// ---------------------------------------------------------------------------

pub struct GraphStore {
    _db: Database,
    conn: Connection<'static>,
    // Cache of prepared UNWIND statements keyed by the exact Cypher text.
    // A fresh prepare() is ~0.5-2 ms; with one bulk call per file per label
    // the 500-file fixture produces ~4000 bulk calls so uncached prepare
    // alone costs multiple seconds. Keyed by Cypher rather than by label
    // because node rows with different property subsets generate different
    // UNWIND statements (File rows without size_bytes, etc.).
    // source: scalability_bench regression observed April 2026 when
    // prepare-per-call was introduced; caching restores linear scaling.
    stmt_cache: RefCell<HashMap<String, PreparedStatement>>,
    // Safety: `_db` is heap-allocated via lbug's C++ bridge (UniquePtr).
    // `Connection` borrows `Database` through a raw pointer on the C++ side,
    // not through Rust's borrow checker. Moving the Rust struct does not
    // invalidate the C++ pointer because `Database.db` is `UnsafeCell<UniquePtr>`
    // — the heap allocation is stable. lbug marks both Database and Connection
    // as Send+Sync. This self-referential pattern is safe here because:
    //   (a) `_db` is never moved out or dropped before `conn`,
    //   (b) lbug's own test suite uses the same stack-lifetime pattern,
    //   (c) struct fields drop in declaration order (conn drops before _db).
}

impl GraphStore {
    /// Opens (or creates) a LadybugDB database at `path`, using the
    /// by-construction `system_config()` (test bound, prod override, or the
    /// issue #25 production default — see that function's doc comment for
    /// the precedence rule).
    pub fn open_or_create(path: &Path) -> Result<Self, String> {
        Self::open_or_create_with_config(path, system_config()?)
    }

    /// Opens (or creates) a LadybugDB database at `path` with an explicit
    /// `SystemConfig`, bypassing `system_config()`'s env-var resolution.
    ///
    /// Since the 8 GiB-cap repeal (2026-08-14) its only caller is
    /// `open_or_create` itself, which always resolves the config through
    /// `system_config()` first — this is the seam between "decide the
    /// config" and "open with it", not an alternate entry point. The tests
    /// that once used it to open at an explicit `max_db_size` were removed
    /// with the property they pinned (see `graph_store::tests` and
    /// `graph_cache`'s amended capacity test).
    pub(crate) fn open_or_create_with_config(
        path: &Path,
        config: SystemConfig,
    ) -> Result<Self, String> {
        let db = match Database::new(path, config.clone()) {
            Ok(db) => db,
            Err(first) => {
                // issue #201: a graph left behind by a previous database with
                // the same name refuses to open forever — the only documented
                // remedy was a human `rm -rf`. Self-heal the one condition
                // where that is provably safe, and only that one.
                Self::recover_from_stale_sidecars(path, &first.to_string())?;
                Database::new(path, config).map_err(|e| {
                    crate::write_diagnostics::annotate_write_failure(
                        path,
                        &format!(
                            "lbug database open failed after quarantining stale \
                             sidecars (first attempt: {first}): {e}"
                        ),
                    )
                })?
            }
        };
        // Safety: see comment on the struct. The Database is heap-stable and
        // outlives the Connection because struct fields drop in declaration order.
        let conn: Connection<'static> = unsafe {
            std::mem::transmute::<Connection<'_>, Connection<'static>>(
                Connection::new(&db).map_err(|e| format!("lbug connection failed: {e}"))?,
            )
        };
        Ok(GraphStore {
            _db: db,
            conn,
            stmt_cache: RefCell::new(HashMap::new()),
        })
    }

    /// Creates the full 3a schema (node tables + relationship tables).
    pub fn create_schema(&self) -> Result<(), String> {
        for ddl in node_table_ddl() {
            self.exec_ddl(&ddl)?;
        }
        for ddl in rel_table_ddl() {
            self.exec_ddl(&ddl)?;
        }
        Ok(())
    }

    /// Executes an arbitrary Cypher query and returns columns + rows.
    ///
    /// Intentionally UNBOUNDED. This is a shared internal primitive: 70+ callers
    /// (resolvers, the indexer dedup pass, `node_count`/`edge_count`, history
    /// persist) require the complete result set, and silently `.take(n)`-ing
    /// here would corrupt graph resolution rather than just trim a response.
    /// Per the bounded-I/O plan the host-cap bound belongs at the MCP response
    /// boundary, not on this primitive — see `crate::response_budget` and the
    /// LIMIT injection in `do_query_graph`, the byte-budget caps in
    /// `do_get_impact` / `do_get_processes`, and the per-relation LIMITs in
    /// `search::find_related_out` / `find_related_in`.
    pub fn execute_query(&self, cypher: &str) -> Result<QueryResult, String> {
        Ok(drain_result(&mut self.run(cypher)?))
    }

    /// Executes untrusted, user-supplied Cypher (the `query_graph` tool) under
    /// two engine-authoritative guards the lexical keyword blocklist in
    /// `query_handlers` cannot provide (fleet-watch#15):
    ///
    ///   1. Database-write enforcement via lbug's own
    ///      `PreparedStatement::is_read_only`. `prepare` parses, binds and
    ///      plans without executing; the engine then reports whether the
    ///      statement mutates the database. This catches every write and DDL
    ///      from the compiled plan rather than from a keyword scan, so a
    ///      mutation spelled in syntax the lexical layer does not enumerate is
    ///      still refused.
    ///   2. A wall-clock query timeout, so a pathological plan (e.g. an
    ///      unbounded variable-length `MATCH (a)-[*]->(b)` whose `LIMIT` caps
    ///      rows, not traversal work) cannot pin the single-threaded MCP
    ///      worker indefinitely. It is applied before `prepare` so that
    ///      binding and planning — not only execution — are bounded.
    ///
    /// SCOPE — what this guard does NOT cover. `is_read_only` answers
    /// "does this mutate the DATABASE", not "does this touch the filesystem".
    /// lbug's `StatementReadWriteAnalyzer` overrides `visitCopyFrom` but
    /// leaves `visitCopyTo`, `visitExportDatabase`, `visitImportDatabase` and
    /// `visitAttachDatabase` at the base visitor's no-op, so all four are
    /// classified read-only. Measured 2026-08-24 against lbug 0.19.1: both on
    /// this path and on a `SystemConfig::read_only(true)` handle — which
    /// reaches the SAME predicate through `ClientContext::validateTransaction`
    /// — `COPY (..) TO 'f.csv'` and `EXPORT DATABASE 'd'` execute and write
    /// the filesystem. Those statements are refused upstream by the lexical
    /// gate (`query_handlers::FORBIDDEN_CYPHER_KEYWORDS`), which is therefore
    /// load-bearing rather than redundant; the two layers cover disjoint
    /// families. Pinned by `engine_gate_does_not_cover_filesystem_writes`.
    ///
    /// Single statement only: `prepare` refuses a `;`-chained request
    /// ("We do not support prepare multiple statements"). A trailing `;` is
    /// accepted. `query_handlers` turns a chain into an explicit reason code.
    ///
    /// The timeout is scoped to this call and reset to the engine default
    /// (`0` = disabled) on every exit path, so it cannot leak onto the shared
    /// cached handle that write tools (`ingest_traces`) reuse — an aborted
    /// mid-write ingestion is the failure this ordering exists to prevent.
    ///
    /// source: lbug 0.19.1 `PreparedStatement::is_read_only` (connection.rs:56),
    /// `Connection::set_query_timeout` (connection.rs:360),
    /// `lbug-src/src/include/parser/visitor/statement_read_write_analyzer.h`,
    /// `lbug-src/src/main/client_context.cpp:520-521`.
    pub fn execute_read_only_query(
        &self,
        cypher: &str,
        timeout_ms: u64,
    ) -> Result<QueryResult, String> {
        // Bound plan+bind+execute, and reset on every exit path below.
        self.conn.set_query_timeout(timeout_ms);
        let prepared = self.prepare_read_only(cypher);
        let collected = match prepared {
            Ok(mut stmt) => self.collect_prepared(&mut stmt, cypher),
            Err(e) => Err(e),
        };
        self.conn.set_query_timeout(0);
        collected
    }

    /// Prepares `cypher` and refuses it unless the engine classifies the
    /// compiled plan as non-mutating. Split out so the timeout set/reset pair
    /// around it stays a single straight-line sequence.
    fn prepare_read_only(&self, cypher: &str) -> Result<PreparedStatement, String> {
        let stmt = self
            .conn
            .prepare(cypher)
            .map_err(|e| format!("query failed [{cypher}]: {e}"))?;
        if !stmt.is_read_only() {
            return Err("read_only_query_required: statement mutates the \
                        database (write or DDL) and is refused on the \
                        read-only query path"
                .to_string());
        }
        Ok(stmt)
    }

    /// Executes an already-prepared, parameterless statement and drains its
    /// rows into a `QueryResult`. Split out of `execute_read_only_query` so the
    /// timeout reset around it stays a single straight-line pair.
    fn collect_prepared(
        &self,
        stmt: &mut PreparedStatement,
        cypher: &str,
    ) -> Result<QueryResult, String> {
        let mut result = self
            .conn
            .execute(stmt, Vec::<(&str, Value)>::new())
            .map_err(|e| format!("query failed [{cypher}]: {e}"))?;
        Ok(drain_result(&mut result))
    }

    /// Returns the total number of nodes across all node tables.
    pub fn node_count(&self) -> Result<u64, String> {
        let mut total: u64 = 0;
        for label in NODE_LABELS {
            let cypher = format!("MATCH (n:{label}) RETURN count(n)");
            match self.run(&cypher) {
                Ok(mut r) => {
                    if let Some(row) = r.next() {
                        total += value_to_u64(&row[0]);
                    }
                }
                Err(_) => continue,
            }
        }
        Ok(total)
    }

    /// Returns the total number of edges across all relationship tables.
    pub fn edge_count(&self) -> Result<u64, String> {
        let mut total: u64 = 0;
        for &(rel, _, _) in REL_TABLES {
            let cypher = format!("MATCH ()-[r:{rel}]->() RETURN count(r)");
            match self.run(&cypher) {
                Ok(mut r) => {
                    if let Some(row) = r.next() {
                        total += value_to_u64(&row[0]);
                    }
                }
                Err(_) => continue,
            }
        }
        Ok(total)
    }

    // -- private helpers ----------------------------------------------------

    fn exec_ddl(&self, ddl: &str) -> Result<(), String> {
        self.conn
            .query(ddl)
            .map_err(|e| format!("DDL failed [{ddl}]: {e}"))?;
        Ok(())
    }

    fn run(&self, cypher: &str) -> Result<lbug::QueryResult<'_>, String> {
        self.conn
            .query(cypher)
            .map_err(|e| format!("query failed [{cypher}]: {e}"))
    }

    /// Runs one UNWIND execute() against the cached prepared statement for
    /// `cypher`, preparing and inserting into the cache on first use.
    /// The cache is critical: the per-call prepare() cost dominates small
    /// bulk chunks (common when indexing small files), and caching turns
    /// the whole bulk path into a single plan-once/execute-many loop.
    fn run_prepared(&self, cypher: &str, rows: Value) -> Result<(), String> {
        let mut cache = self.stmt_cache.borrow_mut();
        if !cache.contains_key(cypher) {
            let stmt = self
                .conn
                .prepare(cypher)
                .map_err(|e| format!("prepare failed [{cypher}]: {e}"))?;
            cache.insert(cypher.to_string(), stmt);
        }
        let stmt = cache
            .get_mut(cypher)
            .expect("statement just inserted into cache");
        self.conn
            .execute(stmt, vec![("rows", rows)])
            .map(|_| ())
            .map_err(|e| format!("execute [{cypher}]: {e}"))
    }

    /// Generalization of `run_prepared` for statements with more than one
    /// (or a non-`rows`-named) parameter — currently only
    /// `insert_file_content`'s named-field `CREATE`. Shares the same
    /// prepared-statement cache: identical cypher text across calls, only
    /// the bound values differ.
    fn run_prepared_params(&self, cypher: &str, params: Vec<(&str, Value)>) -> Result<(), String> {
        let mut cache = self.stmt_cache.borrow_mut();
        if !cache.contains_key(cypher) {
            let stmt = self
                .conn
                .prepare(cypher)
                .map_err(|e| format!("prepare failed [{cypher}]: {e}"))?;
            cache.insert(cypher.to_string(), stmt);
        }
        let stmt = cache
            .get_mut(cypher)
            .expect("statement just inserted into cache");
        self.conn
            .execute(stmt, params)
            .map(|_| ())
            .map_err(|e| format!("execute [{cypher}]: {e}"))
    }
}

#[cfg(test)]
mod read_only_tests;
#[cfg(test)]
mod tests;

/// Drains an engine result set into the owned [`QueryResult`] the rest of the
/// crate speaks: the column names, then every row rendered through
/// `value_to_string`.
///
/// Shared by the two paths that materialize a result — `execute_query` (ad-hoc
/// Cypher) and `collect_prepared` (the read-only prepared path) — so a change
/// to how a row is rendered cannot land on one and miss the other.
fn drain_result(result: &mut lbug::QueryResult<'_>) -> QueryResult {
    let columns = result.get_column_names();
    let rows: Vec<Vec<String>> = result
        .by_ref()
        .map(|tuple| tuple.iter().map(value_to_string).collect())
        .collect();
    QueryResult { columns, rows }
}
