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

mod config;
mod ddl;
mod schema;
mod serialize;
pub use config::*;
use ddl::*;
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
// Node labels — source: stages/stage-3.md §schema (Shannon spec, 3a subset)
// ---------------------------------------------------------------------------

pub const NODE_DIRECTORY: &str = "Directory"; // source: stages/stage-3.md §schema
pub const NODE_FILE: &str = "File"; // source: stages/stage-3.md §schema
pub const NODE_MODULE: &str = "Module"; // source: stages/stage-3.md §schema
pub const NODE_FUNCTION: &str = "Function"; // source: stages/stage-3.md §schema
pub const NODE_METHOD: &str = "Method"; // source: stages/stage-3.md §schema
pub const NODE_STRUCT: &str = "Struct"; // source: stages/stage-3.md §schema
pub const NODE_ENUM: &str = "Enum"; // source: stages/stage-3.md §schema
pub const NODE_VARIANT: &str = "Variant"; // source: stages/stage-3.md §schema
pub const NODE_TRAIT: &str = "Trait"; // source: stages/stage-3.md §schema
pub const NODE_FIELD: &str = "Field"; // source: stages/stage-3.md §schema
pub const NODE_CONSTANT: &str = "Constant"; // source: stages/stage-3.md §schema
pub const NODE_TYPE_ALIAS: &str = "TypeAlias"; // source: stages/stage-3.md §schema
pub const NODE_IMPORT: &str = "Import"; // source: stages/stage-3.md §schema
pub const NODE_CALL_SITE: &str = "CallSite"; // source: stages/stage-3.md §schema
pub const NODE_COMMUNITY: &str = "Community"; // source: stages/stage-3c.md §4.1
pub const NODE_PROCESS: &str = "Process"; // source: stages/stage-3c.md §4.1
pub const NODE_STDLIB_SYMBOL: &str = "StdlibSymbol"; // source: stages/stage-3b-v2.md §5 Layer 5

// History layer — temporal axis over the structural snapshot.
// source: second-brain history requirement — the graph must track not just
// the current state of an entity but its evolution: which commits touched it,
// and the chain of its successive versions. A `Commit` is a point in git
// history; a `Version` is one revision of an entity (a File or a symbol) as it
// stood at a particular commit. The structural graph remains the HEAD
// snapshot; the version spine hangs off it via VersionOf/ChangedIn edges so
// every entity stays traversable in both directions across time.
pub const NODE_COMMIT: &str = "Commit";
pub const NODE_VERSION: &str = "Version";

// Infrastructure-as-code layer (issue #63) — deployment surface as first-class
// graph material, mirroring DeusData/codebase-memory-mcp's pass_k8s.c (Resource
// nodes per K8s kind, Module nodes per Kustomize overlay) and pass_infrascan.c
// (Dockerfile base image / stages / ports). AP adds these ALONGSIDE the existing
// File node for each manifest (the File already exists post-index); the IaC pass
// enriches, never replaces. source: issue #63 acceptance criteria 1-3.
//
// IacResource — one node per K8s manifest document (Deployment/Service/ConfigMap/
// …) AND per Dockerfile build target. `resource_kind` discriminates the concrete
// kind ("Deployment", "Dockerfile", …); `source` is "k8s" | "dockerfile". The id
// is `<file-rel>::<discriminator>` so the incremental pass's per-file symbol
// purge (`starts_with(id, "<rel>::")`) reclaims it on reparse with zero new purge
// code (issue #62 integration).
pub const NODE_IAC_RESOURCE: &str = "IacResource";
// IacModule — one node per Kustomize overlay (kustomization.yaml). Mirrors CBM's
// "Module" node; renamed to avoid colliding with the code `Module` label.
pub const NODE_IAC_MODULE: &str = "IacModule";
// IacImage — a container image reference (Dockerfile `FROM`, K8s container
// `image:`). Shared/deduplicated external-ref node (like StdlibSymbol): keyed by
// the normalized reference string, never per-file, so it is not purged per file.
pub const NODE_IAC_IMAGE: &str = "IacImage";

// ---------------------------------------------------------------------------
// Edge kinds — source: stages/stage-3.md §schema (Shannon spec, 3a subset)
// ---------------------------------------------------------------------------

#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_CONTAINS: &str = "Contains"; // source: stages/stage-3.md §schema
#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_DEFINES: &str = "Defines"; // source: stages/stage-3.md §schema
#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_HAS_METHOD: &str = "HasMethod"; // source: stages/stage-3.md §schema
#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_HAS_FIELD: &str = "HasField"; // source: stages/stage-3.md §schema
#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_HAS_VARIANT: &str = "HasVariant"; // source: stages/stage-3.md §schema

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

// ---------------------------------------------------------------------------
// Stale-sidecar recovery — source: issue #201.
//
// lbug writes temporary sidecars next to the database: `<db>.wal`,
// `<db>.wal.checkpoint`, `<db>.shadow`, `<db>.tmp`. Each carries the UUID of
// the database that produced it, and `FileDBIDUtils::verifyDatabaseID`
// (lbug-src/src/storage/file_db_id_utils.cpp) throws when it does not match
// the database being opened:
//
//   "Database ID for temporary file '<path>' does not match the current
//    database. This file may have been left behind from a previous database
//    with the same name. If it is safe to do so, please delete this file and
//    restart the database."
//
// Observed on a June-vintage graph dir in the 2026-08-06 A/B ingestion bench:
// every subsequent open failed hard and the only remedy was deleting the whole
// directory by hand.
//
// The throw sites are wal_replayer.cpp and shadow_file.cpp — the SIDECARS, never
// the main database file. So recovery does not need to discard the index: move
// the mismatched sidecars aside and reopen. A sidecar whose UUID belongs to a
// different database describes transactions that are not this database's, so
// replaying it would be wrong; discarding it is what upstream advises.
//
// Deliberately narrow. The recovery runs ONLY for this error signature: a full
// disk or a permission fault must never be "fixed" by removing files, so every
// other open failure keeps its existing annotate_write_failure path.
// ---------------------------------------------------------------------------

/// Temporary sidecar suffixes lbug appends to the database path.
/// Source: lbug-src/src/include/common/constants.h StorageConstants
/// (WAL_FILE_SUFFIX / CHECKPOINT_WAL_FILE_SUFFIX / SHADOWING_SUFFIX /
/// TEMP_FILE_SUFFIX).
const LBUG_SIDECAR_SUFFIXES: &[&str] = &["wal", "wal.checkpoint", "shadow", "tmp"];

/// True iff `err` is lbug's stale-sidecar database-ID mismatch.
///
/// Matches on two co-occurring fragments of the upstream message rather than
/// the whole string, so incidental rewording upstream does not silently turn
/// recovery off, while an unrelated error cannot accidentally match.
pub(crate) fn is_stale_sidecar_db_id_error(err: &str) -> bool {
    err.contains("Database ID") && err.contains("does not match")
}

impl GraphStore {
    /// Moves mismatched lbug sidecars aside so the database can be reopened.
    ///
    /// Returns Ok(()) when the caller should retry the open. Returns Err when
    /// the error was not the stale-sidecar signature (the caller must surface
    /// the original failure unchanged) or when nothing could be quarantined.
    ///
    /// Quarantine, not delete: the sidecars are renamed to
    /// `<name>.stale-<unix_millis>` beside the database. They are small, the
    /// operation is reversible, and a wrong call is then recoverable — which a
    /// delete would not be. Every action is logged with the reason.
    fn recover_from_stale_sidecars(path: &Path, err: &str) -> Result<(), String> {
        if !is_stale_sidecar_db_id_error(err) {
            // Not our condition — hand the original error back untouched so
            // ENOSPC/permission diagnostics keep their existing contract.
            return Err(crate::write_diagnostics::annotate_write_failure(
                path,
                &format!("lbug database open failed: {err}"),
            ));
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut moved = 0usize;
        for suffix in LBUG_SIDECAR_SUFFIXES {
            let mut sidecar = path.as_os_str().to_owned();
            sidecar.push(format!(".{suffix}"));
            let sidecar = std::path::PathBuf::from(sidecar);
            // symlink_metadata: never follow a symlink planted at the sidecar
            // path, and only ever touch a regular file.
            match std::fs::symlink_metadata(&sidecar) {
                Ok(meta) if meta.is_file() => {
                    let mut dest = sidecar.as_os_str().to_owned();
                    dest.push(format!(".stale-{stamp}"));
                    let dest = std::path::PathBuf::from(dest);
                    match std::fs::rename(&sidecar, &dest) {
                        Ok(()) => {
                            moved += 1;
                            eprintln!(
                                "[ap] graph recovery: {} carried a foreign database ID \
                                 (left by a previous database of the same name); moved to {} \
                                 and reopening — the index itself is untouched (issue #201)",
                                sidecar.display(),
                                dest.display()
                            );
                        }
                        Err(e) => {
                            return Err(format!(
                                "graph recovery: could not quarantine stale sidecar {}: {e}",
                                sidecar.display()
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        if moved == 0 {
            // The signature said stale sidecar but none is present next to the
            // database — do not silently retry an open that will fail the same
            // way; say what was looked for.
            return Err(format!(
                "graph recovery: lbug reported a stale-sidecar database-ID mismatch for {} \
                 but no sidecar ({}) was found to quarantine; original error: {err}",
                path.display(),
                LBUG_SIDECAR_SUFFIXES.join(", ")
            ));
        }
        Ok(())
    }
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
    /// `pub(crate)` rather than a test-only cfg: exists solely so tests (in
    /// this module and in `graph_cache`'s test module) can exercise a
    /// specific `max_db_size` — in particular `DEFAULT_PROD_MAX_DB_SIZE_BYTES`
    /// — without racing `.cargo/config.toml`'s process-wide
    /// `AP_LBUG_TEST_MAX_DB_SIZE` override across parallel test threads.
    /// `open_or_create` is the only production call site; it always resolves
    /// through `system_config()` first, so production behavior is unchanged.
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

    /// Inserts a single node. `properties` are `(key, cypher_literal)` pairs.
    /// Values are interpolated as-is into Cypher — caller must quote strings.
    pub fn insert_node(&self, label: &str, properties: &[(&str, &str)]) -> Result<(), String> {
        let props = format_props(properties);
        let cypher = format!("CREATE (:{label} {{{props}}})");
        self.run(&cypher)?;
        Ok(())
    }

    /// Bulk-inserts many nodes of the same label using the UNWIND + prepared
    /// statement pattern with typed `LogicalType::Struct` parameters.
    ///
    /// Strategy: one prepared statement per unique (label, property-subset)
    /// combination, cached on the `GraphStore`, executed per chunk of
    /// BULK_BATCH_SIZE rows. Each chunk flows through the FFI as a
    /// `Value::List(Struct{...}, rows)` — no Cypher string interpolation,
    /// no per-row parse/plan.
    ///
    /// Note: per-call explicit BEGIN/COMMIT was tried and measured slower
    /// on the 500-file fixture (72s vs 38s) because lbug already uses
    /// per-query auto-commit and each explicit tx adds two round-trips.
    /// dba's 8x figure came from wrapping many small writes inside ONE tx,
    /// not from wrapping every bulk call in its own tx.
    ///
    /// source: dba probe_2 in tests/lbug_bulk_investigation.rs confirmed
    /// list-of-structs UNWIND works and is dramatically faster than
    /// per-row CREATE (38x measured on large edge batches).
    pub fn bulk_insert_nodes(
        &self,
        label: &str,
        rows: &[Vec<(String, String)>],
    ) -> Result<u64, String> {
        if rows.is_empty() {
            return Ok(0);
        }
        let schema = node_column_types(label)?;
        let prop_order = node_prop_order(rows, schema);
        let (cypher, row_type) = build_node_unwind(label, &prop_order);
        let mut inserted: u64 = 0;
        for chunk in rows.chunks(BULK_BATCH_SIZE) {
            let values = build_struct_rows(chunk, &prop_order)?;
            let list = Value::List(row_type.clone(), values);
            self.run_prepared(&cypher, list)
                .map_err(|e| format!("bulk_insert_nodes execute: {e}"))?;
            inserted += chunk.len() as u64;
        }
        Ok(inserted)
    }

    /// Bulk-inserts many edges that share the same relationship table using
    /// UNWIND + MATCH + CREATE in a single prepared statement per chunk.
    /// Each edge is `(from_id, to_id, properties)`.
    ///
    /// Strategy mirrors bulk_insert_nodes: one prepared UNWIND statement
    /// per (rel_table, property-subset), cached on the `GraphStore`,
    /// executed per BULK_BATCH_SIZE chunk, values passed as typed
    /// `Value::List(Struct{...}, rows)`. The (from_label, to_label, rel)
    /// triple is known at prep time from REL_TABLES, so one prepared
    /// statement covers every edge of a given kind.
    ///
    /// source: dba probe_4 + probe_9 in tests/lbug_bulk_investigation.rs.
    pub fn bulk_insert_edges(&self, rel_table: &str, edges: &[PropEdge]) -> Result<u64, String> {
        if edges.is_empty() {
            return Ok(0);
        }
        let (from_label, to_label) = parse_rel_endpoints(rel_table)?;
        let prop_schema = edge_column_types(rel_table);
        let prop_order = edge_prop_order(edges, prop_schema);
        let (cypher, row_type) = build_edge_unwind(rel_table, from_label, to_label, &prop_order);
        let mut inserted: u64 = 0;
        for chunk in edges.chunks(BULK_BATCH_SIZE) {
            let values = build_edge_struct_rows(chunk, &prop_order)?;
            let list = Value::List(row_type.clone(), values);
            self.run_prepared(&cypher, list)
                .map_err(|e| format!("bulk_insert_edges execute: {e}"))?;
            inserted += chunk.len() as u64;
        }
        Ok(inserted)
    }

    /// Inserts a single edge between two nodes identified by their `id` property.
    pub fn insert_edge(
        &self,
        rel_type: &str,
        from_id: &str,
        to_id: &str,
        properties: &[(&str, &str)],
    ) -> Result<(), String> {
        let (from_label, to_label) = parse_rel_endpoints(rel_type)?;
        let props_clause = if properties.is_empty() {
            String::new()
        } else {
            format!(" {{{}}}", format_props(properties))
        };
        let from_lit = cypher_str(from_id);
        let to_lit = cypher_str(to_id);
        // source: Kuzu PK-index scan — inline `{id: ..}` avoids the A×B
        // CrossProduct the comma+WHERE form plans (see build_edge_unwind).
        let cypher = format!(
            "MATCH (a:{from_label} {{id: {from_lit}}}) \
             MATCH (b:{to_label} {{id: {to_lit}}}) \
             CREATE (a)-[:{rel_type}{props_clause}]->(b)"
        );
        self.run(&cypher)?;
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
    /// Flips `is_resolved = true` on all nodes of `label` whose id is in `ids`.
    ///
    /// Uses the codebase's prepared-UNWIND convention (parameterized `$rows`, no
    /// Cypher string interpolation of data — mirrors bulk_insert_nodes) so a
    /// codebase with tens of thousands of resolved imports/calls costs one
    /// prepared statement per chunk. `label` is a fixed schema constant
    /// ("Import"/"CallSite"), safe to embed. source: stages/stage-3.md §10.4.
    pub(crate) fn mark_nodes_resolved(&self, label: &str, ids: &[&str]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        // source: Kuzu PK-index scan — inline `{id: rid}` seeks the index per
        // row; the `MATCH (n) WHERE n.id = rid` form scans all N nodes per row
        // (O(rows·N)) on large graphs. Same fix class as the edge queries.
        let cypher =
            format!("UNWIND $rows AS rid MATCH (n:{label} {{id: rid}}) SET n.is_resolved = true");
        for chunk in ids.chunks(BULK_BATCH_SIZE) {
            let values: Vec<Value> = chunk
                .iter()
                .map(|id| Value::String((*id).to_string()))
                .collect();
            let list = Value::List(LogicalType::String, values);
            self.run_prepared(&cypher, list)?;
        }
        Ok(())
    }

    pub fn execute_query(&self, cypher: &str) -> Result<QueryResult, String> {
        let mut result = self.run(cypher)?;
        let columns = result.get_column_names();
        let rows: Vec<Vec<String>> = result
            .by_ref()
            .map(|tuple| tuple.iter().map(value_to_string).collect())
            .collect();
        Ok(QueryResult { columns, rows })
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
}

#[cfg(test)]
mod tests;
