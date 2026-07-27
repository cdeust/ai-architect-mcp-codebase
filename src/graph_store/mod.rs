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
        let db = Database::new(path, config).map_err(|e| {
            // A full disk surfaces here as an opaque lbug abort (e.g.
            // `unordered_map::at: key not found`); annotate it with the real,
            // path-naming storage condition so ENOSPC is distinguishable from
            // a genuine key-not-found (issue #143).
            crate::write_diagnostics::annotate_write_failure(
                path,
                &format!("lbug database open failed: {e}"),
            )
        })?;
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// issue #143: when the store cannot be created because the target's
    /// directory is unwritable (a full disk in production; a non-existent
    /// parent as the portable seam here), `open_or_create` must fail with a
    /// message that names the path and the OS-level write condition — not the
    /// bare opaque lbug abort. Asserts the emission itself (§13.1-F1/A3).
    #[test]
    fn open_or_create_names_the_path_when_the_target_is_unwritable() {
        let target = Path::new("/no-such-root-ai-architect-143/inner/testdb");
        let err = GraphStore::open_or_create(target)
            .err()
            .expect("opening a store under a non-existent root must fail");
        assert!(
            err.contains("/no-such-root-ai-architect-143/inner"),
            "error must name the unwritable directory, got: {err}"
        );
        assert!(
            err.contains("write probe") && err.contains("failed"),
            "error must name the OS-level write condition, got: {err}"
        );
        // The original lbug text is still present for continuity.
        assert!(
            err.contains("lbug database open failed"),
            "underlying store error preserved, got: {err}"
        );
    }

    #[test]
    fn test_create_and_query() {
        // source: issue #21 — a fixed `temp_dir().join("graph_store_test")`
        // path collides under default parallel `cargo test` execution (the
        // embedded DB's file lock races across test threads). tempfile::
        // TempDir allocates a unique-per-call directory (mirrors the #13-fix
        // pattern in tests/lbug_bulk_investigation.rs).
        let dir = tempfile::Builder::new()
            .prefix("graph_store_test")
            .tempdir()
            .expect("create temp dir");
        let db_path = dir.path().join("testdb");

        let store = GraphStore::open_or_create(&db_path).expect("open_or_create");
        store.create_schema().expect("create_schema");

        store
            .insert_node(
                NODE_FUNCTION,
                &[
                    ("id", "'fn1'"),
                    ("name", "'main'"),
                    ("qualified_name", "'crate::main'"),
                    ("start_line", "1"),
                    ("end_line", "10"),
                    ("visibility", "'pub'"),
                    ("is_async", "false"),
                ],
            )
            .expect("insert Function node");

        let qr = store
            .execute_query("MATCH (f:Function) WHERE f.name = 'main' RETURN f.name")
            .expect("query");
        assert_eq!(qr.columns, vec!["f.name"]);
        assert!(!qr.rows.is_empty(), "expected at least one row");
        assert_eq!(qr.rows[0][0], "main");

        let count = store.node_count().expect("node_count");
        assert!(count >= 1, "expected node_count >= 1, got {count}");
    }

    #[test]
    fn test_bulk_insert_nodes_and_edges() {
        // source: issue #21 — unique-per-call TempDir; see test_create_and_query.
        let dir = tempfile::Builder::new()
            .prefix("graph_store_bulk_test")
            .tempdir()
            .expect("create temp dir");
        let db_path = dir.path().join("testdb");

        let store = GraphStore::open_or_create(&db_path).expect("open");
        store.create_schema().expect("schema");

        let mut files: Vec<Vec<(String, String)>> = Vec::new();
        for i in 0..7 {
            let id = format!("f{i}.rs");
            files.push(vec![
                ("id".into(), cypher_str(&id)),
                ("path".into(), cypher_str(&id)),
                ("name".into(), cypher_str(&id)),
                ("extension".into(), cypher_str("rs")),
                ("size_bytes".into(), "0".into()),
            ]);
        }
        let n = store.bulk_insert_nodes("File", &files).expect("bulk nodes");
        assert_eq!(n, 7);

        let mut edges: PropEdgeList = Vec::new();
        for i in 0..6 {
            edges.push((format!("f{i}.rs"), format!("f{}.rs", i + 1), Vec::new()));
        }
        let e = store
            .bulk_insert_edges("Imports_File_File", &edges)
            .expect("bulk edges");
        assert_eq!(e, 6);

        let qr = store
            .execute_query("MATCH (f:File) RETURN count(f)")
            .expect("count");
        let c: u64 = qr.rows[0][0].parse().unwrap_or(0);
        assert_eq!(c, 7);
    }

    #[test]
    fn label_has_qualified_name_matches_the_schema() {
        // Contract: true for exactly the labels whose node table declares a
        // `qualified_name` column (mirrors node_column_types). A read-side
        // traversal binds `n.qualified_name` only when this is true, so a wrong
        // answer either drops rows (false-for-a-qn-label) or crashes the query
        // with a Binder exception (true-for-a-non-qn-label). Assert both arms.
        for yes in [
            NODE_MODULE,
            NODE_FUNCTION,
            NODE_METHOD,
            NODE_STRUCT,
            NODE_ENUM,
            NODE_VARIANT,
            NODE_TRAIT,
            NODE_CONSTANT,
            NODE_TYPE_ALIAS,
            NODE_VERSION,
            NODE_IAC_RESOURCE,
            NODE_IAC_MODULE,
        ] {
            assert!(
                label_has_qualified_name(yes),
                "{yes} declares qualified_name in its DDL"
            );
        }
        for no in [
            NODE_FILE,
            NODE_DIRECTORY,
            NODE_FIELD,
            NODE_IMPORT,
            NODE_CALL_SITE,
            NODE_COMMUNITY,
            NODE_PROCESS,
            NODE_STDLIB_SYMBOL,
            NODE_COMMIT,
            NODE_IAC_IMAGE,
        ] {
            assert!(
                !label_has_qualified_name(no),
                "{no} has no qualified_name column"
            );
        }
    }

    #[test]
    fn test_cypher_str_escape_rules() {
        // Backslash must be escaped FIRST, then quote.
        // Input: `foo'bar`  → literal should contain `\'`.
        assert_eq!(cypher_str("foo'bar"), "'foo\\'bar'");
        // Input: `a\b`      → literal should contain `\\`.
        assert_eq!(cypher_str("a\\b"), "'a\\\\b'");
        // Input: `x\'y` (naive replace('\'', "\\'") would produce `'x\\\\'y'`
        // which re-opens the literal after `\\`). Our rule:
        //   \  -> \\     (escape backslash first)
        //   '  -> \'     (then escape quotes)
        // So `x\'y` becomes `x\\\'y` inside the quotes.
        assert_eq!(cypher_str("x\\'y"), "'x\\\\\\'y'");
    }

    #[test]
    fn test_cypher_injection_rejected() {
        // An id containing an unescaped single quote used to allow a caller
        // to inject arbitrary Cypher (including DETACH DELETE). After the
        // C1 fix, the injection attempt becomes an ordinary string literal
        // that round-trips through the DB safely.
        // source: issue #21 — unique-per-call TempDir; see test_create_and_query.
        let dir = tempfile::Builder::new()
            .prefix("graph_store_inject_test")
            .tempdir()
            .expect("create temp dir");
        let db_path = dir.path().join("testdb");

        let store = GraphStore::open_or_create(&db_path).expect("open_or_create");
        store.create_schema().expect("create_schema");

        // Insert two File nodes so insert_edge has something to MATCH.
        store
            .insert_node(
                NODE_FILE,
                &[
                    ("id", &cypher_str("src/a.rs")),
                    ("path", &cypher_str("src/a.rs")),
                    ("name", &cypher_str("a.rs")),
                    ("extension", &cypher_str("rs")),
                    ("size_bytes", "0"),
                ],
            )
            .expect("insert file a");

        // Adversarial id: contains `'` and would-be Cypher payload.
        let evil_id = "src/b.rs' DETACH DELETE n //";
        store
            .insert_node(
                NODE_FILE,
                &[
                    ("id", &cypher_str(evil_id)),
                    ("path", &cypher_str(evil_id)),
                    ("name", &cypher_str("b.rs")),
                    ("extension", &cypher_str("rs")),
                    ("size_bytes", "0"),
                ],
            )
            .expect("insert file b");

        // Edge insert used to be the injection site. Must succeed safely now.
        store
            .insert_edge("Imports_File_File", "src/a.rs", evil_id, &[])
            .expect("insert_edge with quote-containing id must not inject");

        // If injection had worked, DETACH DELETE would have wiped nodes.
        // Verify both File nodes are still present.
        let cnt = store
            .execute_query("MATCH (f:File) RETURN count(f)")
            .expect("count query");
        let count_val: u64 = cnt.rows[0][0].parse().unwrap_or(0);
        assert_eq!(count_val, 2, "injection attempt must not delete nodes");
    }

    // -----------------------------------------------------------------
    // issue #25 — max_db_size validation and the production default.
    // Pure-function tests: no env var mutation, so these are safe under
    // cargo test's default parallel (threaded) execution alongside every
    // other test in this binary.
    // -----------------------------------------------------------------

    #[test]
    fn max_db_size_rejects_non_numeric() {
        let err = parse_and_validate_max_db_size("not-a-number", "AP_LBUG_TEST_MAX_DB_SIZE")
            .expect_err("non-numeric value must be rejected");
        assert!(
            err.contains("AP_LBUG_TEST_MAX_DB_SIZE"),
            "error must name the offending var: {err}"
        );
    }

    #[test]
    fn max_db_size_rejects_below_lbug_floor() {
        // 4 MiB is below the 8 MiB floor lbug's verifySizeParams enforces.
        let err = parse_and_validate_max_db_size("4194304", "AP_LBUG_MAX_DB_SIZE")
            .expect_err("below-floor value must be rejected");
        assert!(
            err.contains("minimum"),
            "error must explain the floor: {err}"
        );
    }

    #[test]
    fn max_db_size_rejects_non_power_of_two() {
        // 8 MiB + 1 byte: above the floor but not a power of two.
        let err = parse_and_validate_max_db_size("8388609", "AP_LBUG_MAX_DB_SIZE")
            .expect_err("non-power-of-two value must be rejected");
        assert!(
            err.contains("power of two"),
            "error must explain the constraint: {err}"
        );
    }

    #[test]
    fn max_db_size_accepts_valid_power_of_two() {
        // 512 MiB — the existing test bound (.cargo/config.toml).
        let bytes = parse_and_validate_max_db_size("536870912", "AP_LBUG_TEST_MAX_DB_SIZE")
            .expect("valid power-of-two above the floor must be accepted");
        assert_eq!(bytes, 536_870_912);
    }

    #[test]
    // clippy::assertions_on_constants: both operands are `const` today, so
    // clippy can prove this at lint time — but the whole point of this test
    // is to keep proving it if a future edit changes either constant. A
    // `const { assert!(..) }` block would only run at compile time (same
    // blind spot); the ordinary runtime assert here is deliberate, not an
    // oversight.
    #[allow(clippy::assertions_on_constants)]
    fn prod_default_is_valid_per_lbug_constraints() {
        // The production default itself must satisfy the exact constraints
        // parse_and_validate_max_db_size enforces for an operator-supplied
        // override, otherwise system_config()'s fallback would build a
        // SystemConfig that lbug's own verifySizeParams rejects at open time.
        assert!(
            DEFAULT_PROD_MAX_DB_SIZE_BYTES >= MIN_MAX_DB_SIZE_BYTES,
            "prod default must be at least lbug's 8 MiB floor"
        );
        assert_eq!(
            DEFAULT_PROD_MAX_DB_SIZE_BYTES & (DEFAULT_PROD_MAX_DB_SIZE_BYTES - 1),
            0,
            "prod default must be a power of two"
        );
        assert_eq!(
            DEFAULT_PROD_MAX_DB_SIZE_BYTES,
            8 * 1024 * 1024 * 1024,
            "prod default must be exactly 8 GiB"
        );
    }

    #[test]
    fn prod_default_config_opens_a_real_database() {
        // Proves DEFAULT_PROD_MAX_DB_SIZE_BYTES is not just internally
        // consistent (prod_default_is_valid_per_lbug_constraints, above) but
        // actually accepted by lbug's C++ BufferManager::verifySizeParams.
        // Built directly via SystemConfig rather than through
        // system_config(), because .cargo/config.toml's [env] table sets
        // AP_LBUG_TEST_MAX_DB_SIZE for every cargo-spawned process (issue
        // #21/#24) and mutating that process-wide var at runtime here would
        // race other tests in this binary — see graph_cache.rs's
        // `prod_default_bound_opens_max_cached_graphs_simultaneously` for the
        // multi-open concurrency proof at this exact bound.
        let dir = tempfile::Builder::new()
            .prefix("graph_store_prod_default_test")
            .tempdir()
            .expect("create temp dir");
        let cfg = SystemConfig::default().max_db_size(DEFAULT_PROD_MAX_DB_SIZE_BYTES);
        let _store = GraphStore::open_or_create_with_config(&dir.path().join("testdb"), cfg)
            .expect("prod default max_db_size must be accepted by lbug");
    }
}
