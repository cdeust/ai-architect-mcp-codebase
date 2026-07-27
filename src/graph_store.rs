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

/// Environment variable that bounds lbug's `max_db_size` (bytes; must be a
/// power of two — see `BufferManager::verifySizeParams` in the vendored
/// `lbug-0.15.4/lbug-src/src/storage/buffer_manager/buffer_manager.cpp`).
/// Unset in production; set once in `.cargo/config.toml`'s `[env]` table so
/// every `cargo test` process (unit tests AND every integration-test binary
/// under `tests/`) picks it up automatically without any test file needing
/// to opt in. Takes priority over `PROD_MAX_DB_SIZE_ENV` (see
/// `system_config`'s doc comment for the precedence rule) so this test-only
/// knob keeps controlling `cargo test` behavior unchanged by issue #25.
///
/// Root cause this bounds: `SystemConfig::default()` leaves `max_db_size`
/// at its sentinel value `u64::from(u32::MAX)` (lbug-0.15.4/src/database.rs:71).
/// lbug's C++ core treats that exact sentinel as "unset" and substitutes
/// `DEFAULT_VM_REGION_MAX_SIZE = 1 << 43` (8 TiB) — see
/// `lbug-0.15.4/lbug-src/src/main/database.cpp:82-83` and the doc comment
/// on `maxDBSize` in `lbug-0.15.4/lbug-src/src/include/main/database.h:50-54`.
/// Each `Database::new`/`GraphStore::open_or_create` call therefore reserves
/// 8 TiB of virtual address space; dozens of concurrent test binaries under
/// `cargo test --workspace` exhaust the process's address space
/// probabilistically (observed: `lbug database open failed: Mmap for size
/// 8796093022208 failed` in `tests/stage6_integration.rs`, run 5 of a 10-run
/// soak, 2026-07-15).
pub const TEST_MAX_DB_SIZE_ENV: &str = "AP_LBUG_TEST_MAX_DB_SIZE";

/// Environment variable that bounds lbug's `max_db_size` for production
/// (non-`cargo test`) processes — bytes, must be a power of two, same
/// `verifySizeParams` constraint as `TEST_MAX_DB_SIZE_ENV`. Unset by default,
/// in which case `DEFAULT_PROD_MAX_DB_SIZE_BYTES` applies (issue #25: the
/// prior behavior left lbug's own 8 TiB sentinel default in place for every
/// production `GraphStore::open_or_create`/`GraphCache` open — up to
/// `graph_cache::MAX_CACHED_GRAPHS` (8) concurrently cached stores × 8 TiB =
/// 64 TiB of virtual address space reserved in the worst case).
pub const PROD_MAX_DB_SIZE_ENV: &str = "AP_LBUG_MAX_DB_SIZE";

/// Minimum accepted `max_db_size` for either env var, mirroring lbug's own
/// floor: `BufferManager::verifySizeParams` rejects
/// `maxDBSize < 2 * LBUG_PAGE_SIZE * PAGE_GROUP_SIZE`
/// (`lbug-0.15.4/lbug-src/src/storage/buffer_manager/buffer_manager.cpp:99-112`)
/// where `LBUG_PAGE_SIZE = 4096` (`PAGE_SIZE_LOG2 = 12`, generated
/// `system_config.h`) and `PAGE_GROUP_SIZE = 1024` (`PAGE_GROUP_SIZE_LOG2 = 10`,
/// `lbug-src/src/include/common/constants.h:74-75`) — `2 * 4096 * 1024` =
/// 8 MiB. Rejecting below this in Rust surfaces an actionable AP-level error
/// message instead of the opaque C++ `BufferManagerException` lbug would
/// otherwise throw from inside `Database::new`.
const MIN_MAX_DB_SIZE_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB — see doc comment above.

/// Production default for `max_db_size` when `PROD_MAX_DB_SIZE_ENV` is unset.
///
/// Derivation (issue #25): measured every lbug graph-DB file reachable on
/// the development machine that produced this fix (75 distinct graphs across
/// `~/.cache/cortex/code-graphs/*/graph`, `~/.cortex/ap_graph/graph`, and
/// `**/.prd-gen/graphs/*/graph` under active project checkouts — see the
/// full measurement table in the issue #25 PR description). The largest
/// observed file was `repro-cortex-viz-deps/graph` at 495,849,472 bytes
/// (~473 MiB — a cortex-viz index run that included `node_modules`
/// dependencies, the heaviest workload measured). Sizing rule: next power of
/// two ≥ (largest measured × 16), floor 8 GiB. `473 MiB × 16` ≈ 7.39 GiB,
/// which is below the 8 GiB floor, so the floor applies:
/// `8 GiB = 1 << 33 = 8_589_934_592` bytes, already a power of two.
/// source: measured 2026-07-15 on this development machine; see PR body for
/// the `du -k` table. Re-measure and update this constant if a materially
/// larger workload is observed in production.
pub const DEFAULT_PROD_MAX_DB_SIZE_BYTES: u64 = 1 << 33; // 8 GiB — see doc comment above.

/// Parses and validates a `max_db_size` env var value: must be a valid
/// non-negative integer, at least `MIN_MAX_DB_SIZE_BYTES`, and a power of
/// two. `env_name` is the variable name, used only to make the error message
/// actionable (which var to fix, not a generic parse failure).
///
/// precondition: `raw` is the exact string read from `std::env::var`.
/// postcondition: `Ok(bytes)` only for a value lbug's own
/// `BufferManager::verifySizeParams` will accept; `Err` names the specific
/// violation (not-a-number / too small / not-power-of-two) so a misconfigured
/// deployment fails loudly at startup instead of silently falling back to a
/// default the operator did not choose.
fn parse_and_validate_max_db_size(raw: &str, env_name: &str) -> Result<u64, String> {
    let bytes: u64 = raw
        .parse()
        .map_err(|e| format!("{env_name}={raw:?} is not a valid non-negative byte count: {e}"))?;
    if bytes < MIN_MAX_DB_SIZE_BYTES {
        return Err(format!(
            "{env_name}={bytes} is below lbug's minimum max_db_size of \
             {MIN_MAX_DB_SIZE_BYTES} bytes (8 MiB; BufferManager::verifySizeParams)"
        ));
    }
    if bytes & (bytes - 1) != 0 {
        return Err(format!(
            "{env_name}={bytes} is not a power of two, which lbug's \
             BufferManager::verifySizeParams requires"
        ));
    }
    Ok(bytes)
}

/// Builds the `SystemConfig` used by every lbug `Database` this crate opens.
/// The single by-construction choke point for `max_db_size`: both
/// `GraphStore::open_or_create` (used by all production code and by every
/// integration test that goes through `GraphStore`) and the one test file
/// that opens a raw `lbug::Database` directly
/// (`tests/lbug_bulk_investigation.rs`) call this function, so no future
/// call site can reintroduce the unbounded default.
///
/// Precedence: `TEST_MAX_DB_SIZE_ENV` (`AP_LBUG_TEST_MAX_DB_SIZE`), when set,
/// always wins — this preserves the issue #21/#24 test-bounding behavior
/// unchanged. Otherwise `PROD_MAX_DB_SIZE_ENV` (`AP_LBUG_MAX_DB_SIZE`), when
/// set, is used. Otherwise `DEFAULT_PROD_MAX_DB_SIZE_BYTES` (8 GiB) applies —
/// this is the issue #25 fix: production no longer falls through to lbug's
/// own 8 TiB sentinel default. Either env var, if set to a value that fails
/// validation (not a power of two, or below lbug's 8 MiB floor), is rejected
/// with an actionable error rather than silently falling back to a default
/// the operator did not choose.
pub fn system_config() -> Result<SystemConfig, String> {
    let config = SystemConfig::default();
    if let Ok(raw) = std::env::var(TEST_MAX_DB_SIZE_ENV) {
        let bytes = parse_and_validate_max_db_size(&raw, TEST_MAX_DB_SIZE_ENV)?;
        return Ok(config.max_db_size(bytes));
    }
    let bytes = match std::env::var(PROD_MAX_DB_SIZE_ENV) {
        Ok(raw) => parse_and_validate_max_db_size(&raw, PROD_MAX_DB_SIZE_ENV)?,
        Err(_) => DEFAULT_PROD_MAX_DB_SIZE_BYTES,
    };
    Ok(config.max_db_size(bytes))
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
// Schema DDL generators
// ---------------------------------------------------------------------------

const NODE_LABELS: &[&str] = &[
    NODE_DIRECTORY,
    NODE_FILE,
    NODE_MODULE,
    NODE_FUNCTION,
    NODE_METHOD,
    NODE_STRUCT,
    NODE_ENUM,
    NODE_VARIANT,
    NODE_TRAIT,
    NODE_FIELD,
    NODE_CONSTANT,
    NODE_TYPE_ALIAS,
    NODE_IMPORT,
    NODE_CALL_SITE,
    NODE_COMMUNITY,
    NODE_PROCESS,
    NODE_STDLIB_SYMBOL,
    NODE_COMMIT,
    NODE_VERSION,
    // Infrastructure-as-code layer (issue #63).
    NODE_IAC_RESOURCE,
    NODE_IAC_MODULE,
    NODE_IAC_IMAGE,
];

/// Single source of truth for all relationship tables: (name, from, to).
/// Used for DDL generation, endpoint lookup, and edge counting.
/// LadybugDB does not support REL TABLE GROUP in the lbug 0.15.x Rust crate
/// (no references in source). We create one rel table per (source, target) pair
/// with a naming convention: `{Kind}_{From}_{To}`.
pub const REL_TABLES: &[(&str, &str, &str)] = &[
    // Contains — source: stages/stage-3.md §schema
    ("Contains_Dir_File", NODE_DIRECTORY, NODE_FILE),
    ("Contains_Dir_Dir", NODE_DIRECTORY, NODE_DIRECTORY),
    ("Contains_File_Module", NODE_FILE, NODE_MODULE),
    // Defines — source: stages/stage-3.md §schema
    ("Defines_File_Function", NODE_FILE, NODE_FUNCTION),
    ("Defines_File_Struct", NODE_FILE, NODE_STRUCT),
    ("Defines_File_Enum", NODE_FILE, NODE_ENUM),
    ("Defines_File_Trait", NODE_FILE, NODE_TRAIT),
    ("Defines_File_Constant", NODE_FILE, NODE_CONSTANT),
    ("Defines_File_TypeAlias", NODE_FILE, NODE_TYPE_ALIAS),
    // source: B1 fix — Q9/Q14 expect File->Import edges, and resolver
    // also needs to walk a Module->Import parent for mod-nested uses.
    ("Defines_File_Import", NODE_FILE, NODE_IMPORT),
    ("Defines_Module_Import", NODE_MODULE, NODE_IMPORT),
    ("Defines_Module_Function", NODE_MODULE, NODE_FUNCTION),
    ("Defines_Module_Struct", NODE_MODULE, NODE_STRUCT),
    ("Defines_Module_Enum", NODE_MODULE, NODE_ENUM),
    ("Defines_Module_Trait", NODE_MODULE, NODE_TRAIT),
    ("Defines_Module_Constant", NODE_MODULE, NODE_CONSTANT),
    ("Defines_Module_TypeAlias", NODE_MODULE, NODE_TYPE_ALIAS),
    // HasMethod — source: stages/stage-3.md §schema
    ("HasMethod_Struct_Method", NODE_STRUCT, NODE_METHOD),
    ("HasMethod_Enum_Method", NODE_ENUM, NODE_METHOD),
    ("HasMethod_Trait_Method", NODE_TRAIT, NODE_METHOD),
    // HasField — source: stages/stage-3.md §schema
    ("HasField_Struct_Field", NODE_STRUCT, NODE_FIELD),
    ("HasField_Enum_Field", NODE_ENUM, NODE_FIELD),
    // HasVariant — source: stages/stage-3.md §schema
    ("HasVariant_Enum_Variant", NODE_ENUM, NODE_VARIANT),
    // Imports — source: stages/stage-3b.md §2, §3
    ("Imports_File_File", NODE_FILE, NODE_FILE),
    // References — all-file indexing: a non-code file (e.g. Markdown doc)
    // pointing at another file via a relative link `[text](./path)`. Distinct
    // from Imports (code dependency); this is documentation/reference cross-
    // linking so the graph connects docs to the files they describe.
    ("References_File_File", NODE_FILE, NODE_FILE),
    ("Imports_File_Module", NODE_FILE, NODE_MODULE),
    ("Imports_File_Function", NODE_FILE, NODE_FUNCTION),
    ("Imports_File_Method", NODE_FILE, NODE_METHOD),
    ("Imports_File_Struct", NODE_FILE, NODE_STRUCT),
    ("Imports_File_Enum", NODE_FILE, NODE_ENUM),
    ("Imports_File_Trait", NODE_FILE, NODE_TRAIT),
    ("Imports_File_Constant", NODE_FILE, NODE_CONSTANT),
    ("Imports_File_TypeAlias", NODE_FILE, NODE_TYPE_ALIAS),
    ("Imports_Module_Function", NODE_MODULE, NODE_FUNCTION),
    ("Imports_Module_Struct", NODE_MODULE, NODE_STRUCT),
    ("Imports_Module_Enum", NODE_MODULE, NODE_ENUM),
    ("Imports_Module_Trait", NODE_MODULE, NODE_TRAIT),
    ("Imports_Module_Constant", NODE_MODULE, NODE_CONSTANT),
    ("Imports_Module_TypeAlias", NODE_MODULE, NODE_TYPE_ALIAS),
    // Calls — source: stages/stage-3b.md §2, §3
    ("Calls_Function_Function", NODE_FUNCTION, NODE_FUNCTION),
    ("Calls_Function_Method", NODE_FUNCTION, NODE_METHOD),
    ("Calls_Method_Function", NODE_METHOD, NODE_FUNCTION),
    ("Calls_Method_Method", NODE_METHOD, NODE_METHOD),
    // source: Spike B' BUG #12 fix — the parser emits a Defines edge from
    // Function/Method to a CallSite (parser/python.rs:485) but no rel table
    // existed for it, so every insert silently dropped. CallSite nodes were
    // orphans in the graph. Adding these tables restores the linkage.
    ("Defines_Function_CallSite", NODE_FUNCTION, NODE_CALL_SITE),
    ("Defines_Method_CallSite", NODE_METHOD, NODE_CALL_SITE),
    // CallSite → callee — emitted by resolver when the callee resolves.
    ("Calls_CallSite_Function", NODE_CALL_SITE, NODE_FUNCTION),
    ("Calls_CallSite_Method", NODE_CALL_SITE, NODE_METHOD),
    (
        "Calls_CallSite_StdlibSymbol",
        NODE_CALL_SITE,
        NODE_STDLIB_SYMBOL,
    ),
    // Implements — source: stages/stage-3b.md §2, §3
    ("Implements_Struct_Trait", NODE_STRUCT, NODE_TRAIT),
    ("Implements_Enum_Trait", NODE_ENUM, NODE_TRAIT),
    // Extends — source: stages/stage-3b.md §2, §3
    ("Extends_Trait_Trait", NODE_TRAIT, NODE_TRAIT),
    // source: Spike B' BUG #9 fix — Python class inheritance (Cortex uses
    // Struct label for Python classes); resolved by resolve_extends.
    ("Extends_Struct_Struct", NODE_STRUCT, NODE_STRUCT),
    ("Extends_Enum_Enum", NODE_ENUM, NODE_ENUM),
    // Uses — source: stages/stage-3b.md §2, §3
    ("Uses_Function_Struct", NODE_FUNCTION, NODE_STRUCT),
    ("Uses_Function_Enum", NODE_FUNCTION, NODE_ENUM),
    ("Uses_Function_Trait", NODE_FUNCTION, NODE_TRAIT),
    ("Uses_Function_TypeAlias", NODE_FUNCTION, NODE_TYPE_ALIAS),
    ("Uses_Method_Struct", NODE_METHOD, NODE_STRUCT),
    ("Uses_Method_Enum", NODE_METHOD, NODE_ENUM),
    ("Uses_Method_Trait", NODE_METHOD, NODE_TRAIT),
    ("Uses_Method_TypeAlias", NODE_METHOD, NODE_TYPE_ALIAS),
    ("Uses_Struct_Struct", NODE_STRUCT, NODE_STRUCT),
    ("Uses_Struct_Enum", NODE_STRUCT, NODE_ENUM),
    ("Uses_Struct_Trait", NODE_STRUCT, NODE_TRAIT),
    ("Uses_Field_Struct", NODE_FIELD, NODE_STRUCT),
    ("Uses_Field_Enum", NODE_FIELD, NODE_ENUM),
    ("Uses_Field_Trait", NODE_FIELD, NODE_TRAIT),
    ("Uses_Field_TypeAlias", NODE_FIELD, NODE_TYPE_ALIAS),
    // 3b-v2 Layer 5 (stdlib index) + Layer 4 (macro expansion) — source:
    // stages/stage-3b-v2.md §5. Stdlib targets carry resolution_method
    // = "stdlib-index" (confidence 0.95) or "macro-expansion" (0.85).
    (
        "Calls_Function_StdlibSymbol",
        NODE_FUNCTION,
        NODE_STDLIB_SYMBOL,
    ),
    ("Calls_Method_StdlibSymbol", NODE_METHOD, NODE_STDLIB_SYMBOL),
    (
        "Implements_Struct_StdlibSymbol",
        NODE_STRUCT,
        NODE_STDLIB_SYMBOL,
    ),
    (
        "Implements_Enum_StdlibSymbol",
        NODE_ENUM,
        NODE_STDLIB_SYMBOL,
    ),
    // 3c MemberOf — source: stages/stage-3c.md §4.2
    ("MemberOf_Function_Community", NODE_FUNCTION, NODE_COMMUNITY),
    ("MemberOf_Method_Community", NODE_METHOD, NODE_COMMUNITY),
    ("MemberOf_Struct_Community", NODE_STRUCT, NODE_COMMUNITY),
    ("MemberOf_Enum_Community", NODE_ENUM, NODE_COMMUNITY),
    ("MemberOf_Trait_Community", NODE_TRAIT, NODE_COMMUNITY),
    ("MemberOf_Constant_Community", NODE_CONSTANT, NODE_COMMUNITY),
    (
        "MemberOf_TypeAlias_Community",
        NODE_TYPE_ALIAS,
        NODE_COMMUNITY,
    ),
    ("MemberOf_Module_Community", NODE_MODULE, NODE_COMMUNITY),
    // 3c EntryPointOf — source: stages/stage-3c.md §4.2
    ("EntryPointOf_Function_Process", NODE_FUNCTION, NODE_PROCESS),
    ("EntryPointOf_Method_Process", NODE_METHOD, NODE_PROCESS),
    // 3c ParticipatesIn — source: stages/stage-3c.md §4.2
    (
        "ParticipatesIn_Function_Process",
        NODE_FUNCTION,
        NODE_PROCESS,
    ),
    ("ParticipatesIn_Method_Process", NODE_METHOD, NODE_PROCESS),
    // History layer — source: second-brain history requirement.
    // Commit lineage + per-entity version spine. Every edge is read in both
    // directions by the query surface, so a consumer can walk:
    //   entity  <-VersionOf-  Version  -ChangedIn->  Commit  -PreviousVersion->  Commit
    // and the reverse (a commit's changed entities, a version's successor).
    // PreviousVersion_Version_Version chains an entity's own revisions over
    // time; PreviousVersion_Commit_Commit is the commit ancestry (first parent).
    ("PreviousVersion_Commit_Commit", NODE_COMMIT, NODE_COMMIT),
    ("VersionOf_Version_File", NODE_VERSION, NODE_FILE),
    ("VersionOf_Version_Function", NODE_VERSION, NODE_FUNCTION),
    ("VersionOf_Version_Method", NODE_VERSION, NODE_METHOD),
    ("VersionOf_Version_Struct", NODE_VERSION, NODE_STRUCT),
    ("VersionOf_Version_Enum", NODE_VERSION, NODE_ENUM),
    ("VersionOf_Version_Trait", NODE_VERSION, NODE_TRAIT),
    ("ChangedIn_Version_Commit", NODE_VERSION, NODE_COMMIT),
    (
        "PreviousVersion_Version_Version",
        NODE_VERSION,
        NODE_VERSION,
    ),
    // Temporal coupling (issue #58) — Tornhill-style git co-change. One
    // File→File edge per pair that changed together often enough; properties
    // carry the coupling strength. source: Tornhill 2015 (temporal coupling),
    // thresholds from DeusData/codebase-memory-mcp pass_githistory.c.
    ("FILE_CHANGES_WITH", NODE_FILE, NODE_FILE),
    // Runtime-observed calls (issue #58) — ingest_traces creates these where a
    // runtime caller→callee has NO static Calls edge (the divergence signal:
    // runtime truth the static resolver missed). Symbol-level, so one table per
    // (Function|Method)×(Function|Method) callable pair.
    (
        "OBSERVED_CALLS_Function_Function",
        NODE_FUNCTION,
        NODE_FUNCTION,
    ),
    ("OBSERVED_CALLS_Function_Method", NODE_FUNCTION, NODE_METHOD),
    ("OBSERVED_CALLS_Method_Function", NODE_METHOD, NODE_FUNCTION),
    ("OBSERVED_CALLS_Method_Method", NODE_METHOD, NODE_METHOD),
    // Infrastructure-as-code layer (issue #63).
    //
    // Defines_File_Iac* are structural facts (the file literally contains this
    // manifest) — the `Defines_` prefix routes them through the structural-
    // provenance shape (confidence 1.0, "iac-direct"), exactly like File→symbol.
    ("Defines_File_IacResource", NODE_FILE, NODE_IAC_RESOURCE),
    ("Defines_File_IacModule", NODE_FILE, NODE_IAC_MODULE),
    // Imports_* are the reference edges. The `Imports_` prefix is deliberate:
    // `clustering::get_impact` reverse-traverses every `Imports_*` table, so a
    // manifest that references a File/image/base-overlay is reported as a
    // reverse-dependent with zero changes to the impact walker (issue #63
    // criterion 5). All carry (confidence, resolution_method): confidence < 1.0
    // marks the heuristic cross-file name/path resolutions (misses reported, not
    // faked — issue #63 criterion 3); `resolution_method` carries the provenance
    // (e.g. "iac-name-match:ConfigMap/app-config").
    (
        "Imports_IacResource_IacImage",
        NODE_IAC_RESOURCE,
        NODE_IAC_IMAGE,
    ),
    ("Imports_IacResource_File", NODE_IAC_RESOURCE, NODE_FILE),
    ("Imports_IacModule_File", NODE_IAC_MODULE, NODE_FILE),
    (
        "Imports_IacModule_IacModule",
        NODE_IAC_MODULE,
        NODE_IAC_MODULE,
    ),
];

fn node_table_ddl() -> Vec<String> {
    vec![
        // source: stages/stage-3.md §schema
        ddl_node(NODE_DIRECTORY, "id STRING, path STRING, name STRING"),
        // source: stages/stage-3.md §10.5 — `parse_errors` records the count of
        // tree-sitter ERROR/MISSING nodes for this file's parse. A file that
        // parses to few/zero symbols with parse_errors > 0 is a degraded parse
        // (e.g. wrong grammar dialect), not a genuinely empty file; downstream
        // tools must be able to tell the two apart.
        ddl_node(NODE_FILE, "id STRING, path STRING, name STRING, extension STRING, size_bytes INT64, parse_errors INT64"),
        ddl_node(NODE_MODULE, "id STRING, name STRING, qualified_name STRING"),
        // source: Spike B' BUG #5 fix — every symbol-bearing node gets a
        // `language` STRING column populated by the indexer from the file's
        // extension (python/rust/typescript). Previously every symbol came
        // back with `language: None` in the JSON dump.
        // source: issue #92 — `return_type` and `constructed_types` carry the
        // function's return-type annotation and the space-joined set of types it
        // constructs; resolve_uses reads both to emit Uses_Function_<Type> edges.
        // Empty ("") for languages that have not adopted the extraction.
        ddl_node(NODE_FUNCTION,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, is_async BOOLEAN, \
             return_type STRING, constructed_types STRING, language STRING"),
        // source: implements fix — `trait_name` carries the trait a method
        // belongs to in an `impl Trait for Type` block (already extracted by
        // the parser at parser/rust.rs but previously dropped for lack of a
        // column). resolve_implements reads it to emit the Type→Trait edge.
        // source: issue #92 — `return_type`/`constructed_types` as on Function.
        ddl_node(NODE_METHOD,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, is_async BOOLEAN, \
             receiver_type STRING, trait_name STRING, return_type STRING, \
             constructed_types STRING, language STRING"),
        // source: Spike B' BUG #9 fix — `bases STRING` column carries a CSV
        // of unresolved base-class names emitted by the parser. The resolver
        // reads this in resolve_extends, looks each name up in the symbol
        // index, and emits the resolved Extends_X_Y edges. Indexer can't
        // route Extends refs directly because their to_qualified_name is a
        // raw NAME (e.g., "Animal"), not a QN — name→QN resolution happens
        // server-side in the resolver pass after all nodes are indexed.
        //
        // source: implements fix — `implements STRING` is the same mechanism
        // for the implemented-trait/interface names (`#[derive(...)]`, Java
        // `implements`). resolve_implements resolves each name to a local
        // Trait or a stdlib trait. Trait carries the column for schema
        // uniformity but never populates it (a trait implements nothing).
        ddl_node(NODE_STRUCT,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, language STRING, \
             bases STRING, implements STRING"),
        ddl_node(NODE_ENUM,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, language STRING, \
             bases STRING, implements STRING"),
        // source: stages/stage-3.md §10.1 — every symbol carries its source
        // span. The parser already emits start_line/end_line for these nodes;
        // the columns were previously missing so the spans were dropped at persist.
        ddl_node(NODE_VARIANT,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, language STRING"),
        ddl_node(NODE_TRAIT,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, language STRING, \
             bases STRING, implements STRING"),
        ddl_node(NODE_FIELD,
            "id STRING, name STRING, type_annotation STRING, visibility STRING, \
             start_line INT64, end_line INT64, language STRING"),
        ddl_node(NODE_CONSTANT,
            "id STRING, name STRING, qualified_name STRING, type_annotation STRING, \
             start_line INT64, end_line INT64, language STRING"),
        ddl_node(NODE_TYPE_ALIAS,
            "id STRING, name STRING, qualified_name STRING, target_type STRING, \
             start_line INT64, end_line INT64, language STRING"),
        // source: stages/stage-3.md §10.1 (span) + §10.4 (`is_resolved` on Import
        // and CallSite — Stage 4 must distinguish "resolved" from "attempted,
        // failed" from "never attempted"; the indexer writes false, the resolver
        // flips it to true when it emits the resolved edge).
        ddl_node(NODE_IMPORT,
            "id STRING, path STRING, alias STRING, is_glob BOOLEAN, \
             start_line INT64, end_line INT64, is_resolved BOOLEAN, language STRING"),
        ddl_node(NODE_CALL_SITE,
            "id STRING, callee_name STRING, line INT64, col INT64, \
             is_resolved BOOLEAN, language STRING"),
        // 3c Community + Process — source: stages/stage-3c.md §4.1
        ddl_node(NODE_COMMUNITY,
            "id STRING, name STRING, algorithm STRING, \
             resolution_param DOUBLE, member_count INT64, \
             modularity_contribution DOUBLE"),
        ddl_node(NODE_PROCESS,
            "id STRING, name STRING, entry_point_id STRING, \
             entry_kind STRING, entry_confidence DOUBLE, \
             depth INT64, symbol_count INT64"),
        // source: stages/stage-3b-v2.md §5 Layer 5 — StdlibSymbol carries
        // language + canonical_path (= id) + receiver_type + name.
        ddl_node(NODE_STDLIB_SYMBOL,
            "id STRING, name STRING, language STRING, \
             receiver_type STRING, canonical_path STRING"),
        // History layer — source: second-brain history requirement.
        // Commit: one git commit. id = sha. committed_at is unix seconds.
        ddl_node(NODE_COMMIT,
            "id STRING, sha STRING, author STRING, author_email STRING, \
             committed_at INT64, message STRING"),
        // Version: one revision of an entity (File or symbol) at a commit.
        // id = "<entity_id>@<sha>". entity_kind discriminates File/Function/
        // Method/Struct/Enum/Trait so the version spine generalizes to any
        // entity type (code today, documents tomorrow). qualified_name mirrors
        // the entity's qn (or path, for File) for direct lookup.
        ddl_node(NODE_VERSION,
            "id STRING, entity_id STRING, entity_kind STRING, \
             qualified_name STRING, change_type STRING, commit_sha STRING, \
             committed_at INT64, lines_changed INT64"),
        // Infrastructure-as-code layer (issue #63). One wide IacResource shape
        // covers both K8s documents and Dockerfile build targets; a bulk insert
        // binds only the columns actually present per row (see node_prop_order),
        // so k8s-only columns (api_version/namespace) and dockerfile-only columns
        // (ports/entrypoint/workdir) coexist without null-padding.
        // source: issue #63 criteria 1-2; column set mirrors pass_k8s.c manifest
        // fields + pass_infrascan.c cbm_dockerfile_result_t.
        // `qualified_name` mirrors `id` here — it exists so the shared read-side
        // reverse-dependency walker (`clustering::get_impact`, which binds
        // `a.qualified_name` on every Imports_* `from` node) does not fail its
        // binder check on an IaC source node. Without it, lbug rejects the query
        // for the IaC rel tables and the edges are silently dropped from impact.
        ddl_node(NODE_IAC_RESOURCE,
            "id STRING, name STRING, qualified_name STRING, resource_kind STRING, \
             api_version STRING, namespace STRING, image STRING, ports STRING, \
             entrypoint STRING, workdir STRING, source STRING, path STRING, \
             start_line INT64"),
        ddl_node(NODE_IAC_MODULE,
            "id STRING, name STRING, qualified_name STRING, resource_kind STRING, \
             source STRING, path STRING, start_line INT64"),
        ddl_node(NODE_IAC_IMAGE,
            "id STRING, reference STRING, name STRING, tag STRING, registry STRING"),
    ]
}

fn ddl_node(label: &str, columns: &str) -> String {
    format!("CREATE NODE TABLE IF NOT EXISTS {label}({columns}, PRIMARY KEY(id))")
}

/// 3b resolution edge tables carry confidence + resolution_method properties.
/// source: stages/stage-3b.md §2 "Edge properties"
fn is_resolution_rel(name: &str) -> bool {
    name.starts_with("Imports_")
        || name.starts_with("Calls_")
        || name.starts_with("Implements_")
        || name.starts_with("Extends_")
        || name.starts_with("Uses_")
        || name.starts_with("References_")
}

/// Structural edges from the parser (Defines, HasMethod, HasField,
/// HasVariant) are ground-truth AST facts. After Spike B' BUG #4 fix
/// they also carry confidence + resolution_method so downstream
/// consumers see uniform provenance across all edge kinds — structural
/// edges default to (1.0, "direct-ast") at insert time.
///
/// source: Spike B' BUG #4 — audited ap_graph.json had confidence/reason
/// = None on all 67,427 edges including structural ones. Adding the
/// columns + populating defaults at emit time fixes that uniformly.
fn is_structural_provenance_rel(name: &str) -> bool {
    name.starts_with("Defines_")
        || name.starts_with("HasMethod_")
        || name.starts_with("HasField_")
        || name.starts_with("HasVariant_")
}

fn is_entrypoint_rel(name: &str) -> bool {
    name.starts_with("EntryPointOf_")
}

fn is_participates_rel(name: &str) -> bool {
    name.starts_with("ParticipatesIn_")
}

/// Temporal-coupling edge (issue #58): File→File git co-change.
fn is_cochange_rel(name: &str) -> bool {
    name == "FILE_CHANGES_WITH"
}

/// Runtime-observed call edge (issue #58): ingest_traces divergence signal.
fn is_observed_calls_rel(name: &str) -> bool {
    name.starts_with("OBSERVED_CALLS_")
}

/// Symbol-level static Calls tables that ingest_traces annotates with an
/// `observed_count` (the runtime weight on a statically-known call). Excludes
/// the CallSite-level Calls tables — traces are symbol→symbol, not call-site.
pub fn is_observable_static_calls_rel(name: &str) -> bool {
    matches!(
        name,
        "Calls_Function_Function"
            | "Calls_Function_Method"
            | "Calls_Method_Function"
            | "Calls_Method_Method"
    )
}

fn rel_table_ddl() -> Vec<String> {
    REL_TABLES
        .iter()
        .map(|(name, from, to)| {
            if is_cochange_rel(name) {
                // Temporal coupling (issue #58). cochange_count = commits where
                // both files changed; support = min(changes_a, changes_b) (the
                // weaker file's revision count — the coupling denominator);
                // coupling = cochange_count/support (Tornhill's degree, the
                // thresholded metric); jaccard = cochange/(a+b-cochange); and the
                // most recent co-change unix timestamp.
                format!(
                    "CREATE REL TABLE IF NOT EXISTS {name}(\
                     FROM {from} TO {to}, \
                     cochange_count INT64, support INT64, coupling DOUBLE, \
                     jaccard DOUBLE, last_co_change INT64)"
                )
            } else if is_observed_calls_rel(name) {
                // Runtime-observed calls (issue #58): observed_count = how many
                // times the trace saw this caller→callee.
                format!(
                    "CREATE REL TABLE IF NOT EXISTS {name}(\
                     FROM {from} TO {to}, observed_count INT64)"
                )
            } else if is_observable_static_calls_rel(name) {
                // Symbol-level Calls also carry observed_count (issue #58) so
                // ingest_traces can annotate a statically-known call with its
                // runtime weight, in addition to the resolution provenance.
                format!(
                    "CREATE REL TABLE IF NOT EXISTS {name}(\
                     FROM {from} TO {to}, \
                     confidence DOUBLE, resolution_method STRING, observed_count INT64)"
                )
            } else if is_resolution_rel(name) || is_structural_provenance_rel(name) {
                // resolution_rel: stages/stage-3b.md §2.
                // structural_provenance: Spike B' BUG #4 — Defines/HasMethod
                // now also carry (confidence, resolution_method) populated
                // by the indexer as (1.0, "direct-ast") for ground-truth
                // AST facts.
                format!(
                    "CREATE REL TABLE IF NOT EXISTS {name}(\
                     FROM {from} TO {to}, \
                     confidence DOUBLE, resolution_method STRING)"
                )
            } else if is_entrypoint_rel(name) {
                // source: stages/stage-3c.md §4.2
                format!(
                    "CREATE REL TABLE IF NOT EXISTS {name}(\
                     FROM {from} TO {to}, confidence DOUBLE)"
                )
            } else if is_participates_rel(name) {
                // source: stages/stage-3c.md §4.2
                format!(
                    "CREATE REL TABLE IF NOT EXISTS {name}(\
                     FROM {from} TO {to}, depth INT64)"
                )
            } else {
                format!("CREATE REL TABLE IF NOT EXISTS {name}(FROM {from} TO {to})")
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Maps a compound rel table name back to (from_label, to_label).
fn parse_rel_endpoints(rel_type: &str) -> Result<(&str, &str), String> {
    for &(name, from, to) in REL_TABLES.iter() {
        if name == rel_type {
            return Ok((from, to));
        }
    }
    Err(format!("unknown relationship type: {rel_type}"))
}

/// Public schema check — single source of truth for "is this a valid rel
/// table name?" Producers that dynamically format edge type names should
/// validate against this before insertion to avoid schema-mismatch aborts.
pub fn is_known_rel_table(name: &str) -> bool {
    REL_TABLES.iter().any(|(n, _, _)| *n == name)
}

/// Single source of truth for "does this node label declare a `qualified_name`
/// column?" — mirrors `node_column_types`. A read-side traversal that binds
/// `n.qualified_name` MUST gate on this: lbug raises a hard Binder exception
/// (not a NULL) when a query references a property the matched label's table
/// does not declare, which silently drops the whole query's results. Labels
/// without it (File/Directory/Field/Import/CallSite/Community/Process/
/// StdlibSymbol/Commit/IacImage) are matched by `id` alone.
// source: node_table_ddl() — these are exactly the labels whose DDL lists a
// `qualified_name STRING` column.
pub fn label_has_qualified_name(label: &str) -> bool {
    matches!(
        label,
        NODE_MODULE
            | NODE_FUNCTION
            | NODE_METHOD
            | NODE_STRUCT
            | NODE_ENUM
            | NODE_VARIANT
            | NODE_TRAIT
            | NODE_CONSTANT
            | NODE_TYPE_ALIAS
            | NODE_VERSION
            | NODE_IAC_RESOURCE
            | NODE_IAC_MODULE
    )
}

fn format_props(properties: &[(&str, &str)]) -> String {
    properties
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Schema column-type map for the UNWIND bulk path.
//
// The UNWIND + Struct parameter path requires strongly-typed Value variants
// matching each column's declared type. The lookup below mirrors
// node_table_ddl() / rel_table_ddl() exactly — it is the single source of
// truth for "what LogicalType does this (label, property) expect".
// source: stages/stage-3.md §schema, stages/stage-3b.md §2, stages/stage-3c.md §4.
// ---------------------------------------------------------------------------

type ColTypes = &'static [(&'static str, LogicalType)];

// Schema tables, grouped by shape. Mirrors node_table_ddl() columns.
const COLS_DIRECTORY: ColTypes = &[
    ("id", LogicalType::String),
    ("path", LogicalType::String),
    ("name", LogicalType::String),
];
const COLS_FILE: ColTypes = &[
    ("id", LogicalType::String),
    ("path", LogicalType::String),
    ("name", LogicalType::String),
    ("extension", LogicalType::String),
    ("size_bytes", LogicalType::Int64),
    // source: stages/stage-3.md §10.5 — must mirror the NODE_FILE DDL.
    ("parse_errors", LogicalType::Int64),
];
// source: Spike B' BUG #5 + #9 — every symbol-bearing label gets a
// `language` String column; Struct/Enum/Trait additionally gain `bases`.
// Module intentionally has no language (it's a logical aggregation, not
// source); it still uses COLS_MODULE which keeps the pre-Spike-B' shape.
const COLS_MODULE: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
];
const COLS_VARIANT: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    // source: stages/stage-3.md §10.1 — must mirror the NODE_VARIANT DDL.
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
const COLS_FUNCTION: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
    ("is_async", LogicalType::Bool),
    // source: issue #92 — Uses-edge inputs (return type + constructed types).
    ("return_type", LogicalType::String),
    ("constructed_types", LogicalType::String),
    ("language", LogicalType::String),
];
const COLS_METHOD: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
    ("is_async", LogicalType::Bool),
    ("receiver_type", LogicalType::String),
    ("trait_name", LogicalType::String),
    // source: issue #92 — Uses-edge inputs (return type + constructed types).
    ("return_type", LogicalType::String),
    ("constructed_types", LogicalType::String),
    ("language", LogicalType::String),
];
const COLS_TYPEDECL: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
    ("language", LogicalType::String),
    ("bases", LogicalType::String),
    ("implements", LogicalType::String),
];
// source: stages/stage-3.md §10.1 — Field/Constant/TypeAlias/Import gain span
// columns; §10.4 — Import/CallSite gain is_resolved. Each const MUST mirror the
// corresponding node DDL exactly (column name + order feed the UNWIND type map).
const COLS_FIELD: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("type_annotation", LogicalType::String),
    ("visibility", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
const COLS_CONSTANT: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("type_annotation", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
const COLS_TYPE_ALIAS: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("target_type", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
const COLS_IMPORT: ColTypes = &[
    ("id", LogicalType::String),
    ("path", LogicalType::String),
    ("alias", LogicalType::String),
    ("is_glob", LogicalType::Bool),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("is_resolved", LogicalType::Bool),
    ("language", LogicalType::String),
];
const COLS_CALL_SITE: ColTypes = &[
    ("id", LogicalType::String),
    ("callee_name", LogicalType::String),
    ("line", LogicalType::Int64),
    ("col", LogicalType::Int64),
    ("is_resolved", LogicalType::Bool),
    ("language", LogicalType::String),
];
const COLS_COMMUNITY: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("algorithm", LogicalType::String),
    ("resolution_param", LogicalType::Double),
    ("member_count", LogicalType::Int64),
    ("modularity_contribution", LogicalType::Double),
];
const COLS_PROCESS: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("entry_point_id", LogicalType::String),
    ("entry_kind", LogicalType::String),
    ("entry_confidence", LogicalType::Double),
    ("depth", LogicalType::Int64),
    ("symbol_count", LogicalType::Int64),
];
// History layer — mirrors the NODE_COMMIT / NODE_VERSION DDL exactly.
const COLS_COMMIT: ColTypes = &[
    ("id", LogicalType::String),
    ("sha", LogicalType::String),
    ("author", LogicalType::String),
    ("author_email", LogicalType::String),
    ("committed_at", LogicalType::Int64),
    ("message", LogicalType::String),
];
const COLS_VERSION: ColTypes = &[
    ("id", LogicalType::String),
    ("entity_id", LogicalType::String),
    ("entity_kind", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("change_type", LogicalType::String),
    ("commit_sha", LogicalType::String),
    ("committed_at", LogicalType::Int64),
    ("lines_changed", LogicalType::Int64),
];
// Infrastructure-as-code layer (issue #63) — mirror the NODE_IAC_* DDL exactly.
const COLS_IAC_RESOURCE: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("resource_kind", LogicalType::String),
    ("api_version", LogicalType::String),
    ("namespace", LogicalType::String),
    ("image", LogicalType::String),
    ("ports", LogicalType::String),
    ("entrypoint", LogicalType::String),
    ("workdir", LogicalType::String),
    ("source", LogicalType::String),
    ("path", LogicalType::String),
    ("start_line", LogicalType::Int64),
];
const COLS_IAC_MODULE: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("resource_kind", LogicalType::String),
    ("source", LogicalType::String),
    ("path", LogicalType::String),
    ("start_line", LogicalType::Int64),
];
const COLS_IAC_IMAGE: ColTypes = &[
    ("id", LogicalType::String),
    ("reference", LogicalType::String),
    ("name", LogicalType::String),
    ("tag", LogicalType::String),
    ("registry", LogicalType::String),
];

fn node_column_types(label: &str) -> Result<ColTypes, String> {
    match label {
        NODE_DIRECTORY => Ok(COLS_DIRECTORY),
        NODE_FILE => Ok(COLS_FILE),
        NODE_MODULE => Ok(COLS_MODULE),
        NODE_VARIANT => Ok(COLS_VARIANT),
        NODE_FUNCTION => Ok(COLS_FUNCTION),
        NODE_METHOD => Ok(COLS_METHOD),
        NODE_STRUCT | NODE_ENUM | NODE_TRAIT => Ok(COLS_TYPEDECL),
        NODE_FIELD => Ok(COLS_FIELD),
        NODE_CONSTANT => Ok(COLS_CONSTANT),
        NODE_TYPE_ALIAS => Ok(COLS_TYPE_ALIAS),
        NODE_IMPORT => Ok(COLS_IMPORT),
        NODE_CALL_SITE => Ok(COLS_CALL_SITE),
        NODE_COMMUNITY => Ok(COLS_COMMUNITY),
        NODE_PROCESS => Ok(COLS_PROCESS),
        NODE_COMMIT => Ok(COLS_COMMIT),
        NODE_VERSION => Ok(COLS_VERSION),
        NODE_IAC_RESOURCE => Ok(COLS_IAC_RESOURCE),
        NODE_IAC_MODULE => Ok(COLS_IAC_MODULE),
        NODE_IAC_IMAGE => Ok(COLS_IAC_IMAGE),
        other => Err(format!("unknown node label for bulk insert: {other}")),
    }
}

/// Returns the declared property schema for an edge table. Empty for
/// untyped rel tables. source: rel_table_ddl() in this module.
fn edge_column_types(rel_table: &str) -> ColTypes {
    if is_resolution_rel(rel_table) || is_structural_provenance_rel(rel_table) {
        &[
            ("confidence", LogicalType::Double),
            ("resolution_method", LogicalType::String),
        ]
    } else if is_entrypoint_rel(rel_table) {
        &[("confidence", LogicalType::Double)]
    } else if is_participates_rel(rel_table) {
        &[("depth", LogicalType::Int64)]
    } else {
        &[]
    }
}

/// Picks the subset of columns that appear in at least one row, preserving
/// the schema's declared order. Callers may omit columns (e.g. File rows
/// without `size_bytes`) so we only bind what was actually provided.
fn node_prop_order(
    rows: &[Vec<(String, String)>],
    schema: ColTypes,
) -> Vec<(&'static str, LogicalType)> {
    let mut present: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for row in rows {
        for (k, _) in row {
            present.insert(k.as_str());
        }
    }
    schema
        .iter()
        .filter(|(col, _)| present.contains(*col))
        .map(|(col, ty)| (*col, ty.clone()))
        .collect()
}

/// Edge prop order — schema-driven, only bind columns present in data.
fn edge_prop_order(edges: &[PropEdge], schema: ColTypes) -> Vec<(&'static str, LogicalType)> {
    let mut present: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for e in edges {
        for (k, _) in &e.2 {
            present.insert(k.as_str());
        }
    }
    schema
        .iter()
        .filter(|(col, _)| present.contains(*col))
        .map(|(col, ty)| (*col, ty.clone()))
        .collect()
}

fn build_node_unwind(
    label: &str,
    prop_order: &[(&'static str, LogicalType)],
) -> (String, LogicalType) {
    let assigns: Vec<String> = prop_order
        .iter()
        .map(|(k, _)| format!("{k}: row.{k}"))
        .collect();
    let cypher = format!(
        "UNWIND $rows AS row CREATE (:{label} {{{}}})",
        assigns.join(", "),
    );
    let fields: Vec<(String, LogicalType)> = prop_order
        .iter()
        .map(|(k, ty)| ((*k).to_string(), ty.clone()))
        .collect();
    (cypher, LogicalType::Struct { fields })
}

fn build_edge_unwind(
    rel_table: &str,
    from_label: &str,
    to_label: &str,
    prop_order: &[(&'static str, LogicalType)],
) -> (String, LogicalType) {
    let props_clause = if prop_order.is_empty() {
        String::new()
    } else {
        let assigns: Vec<String> = prop_order
            .iter()
            .map(|(k, _)| format!("{k}: row.{k}"))
            .collect();
        format!(" {{{}}}", assigns.join(", "))
    };
    // source: Kuzu primary-key index scan. An inline PK predicate
    // `(n:Label {id: expr})` seeks the PK index (one node); the comma form
    // `MATCH (a:A), (b:B) WHERE a.id=.. AND b.id=..` plans as a CrossProduct
    // over ALL A×B nodes filtered by string Equals. On a full-dependency
    // graph (100k+ nodes) that cross product ran 5h+ at 100% CPU / 10.9G
    // before this fix (measured 2026-07-04, `sample` of hung pid 41120:
    // CrossProduct -> Filter selectUnFlatFlat<string_t,Equals> -> memcmp).
    let cypher = format!(
        "UNWIND $rows AS row \
         MATCH (a:{from_label} {{id: row.from}}) \
         MATCH (b:{to_label} {{id: row.to}}) \
         CREATE (a)-[:{rel_table}{props_clause}]->(b)",
    );
    let mut fields: Vec<(String, LogicalType)> = vec![
        ("from".to_string(), LogicalType::String),
        ("to".to_string(), LogicalType::String),
    ];
    for (k, ty) in prop_order {
        fields.push(((*k).to_string(), ty.clone()));
    }
    (cypher, LogicalType::Struct { fields })
}

fn build_struct_rows(
    chunk: &[Vec<(String, String)>],
    prop_order: &[(&'static str, LogicalType)],
) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(chunk.len());
    for row in chunk {
        let mut fields: Vec<(String, Value)> = Vec::with_capacity(prop_order.len());
        for (col, ty) in prop_order {
            let lit = row.iter().find(|(k, _)| k == *col).map(|(_, v)| v.as_str());
            fields.push(((*col).to_string(), literal_to_value(lit, ty, col)?));
        }
        out.push(Value::Struct(fields));
    }
    Ok(out)
}

fn build_edge_struct_rows(
    edges: &[PropEdge],
    prop_order: &[(&'static str, LogicalType)],
) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(edges.len());
    for (from, to, props) in edges {
        let mut fields: Vec<(String, Value)> = vec![
            ("from".to_string(), Value::String(from.clone())),
            ("to".to_string(), Value::String(to.clone())),
        ];
        for (col, ty) in prop_order {
            let lit = props
                .iter()
                .find(|(k, _)| k == *col)
                .map(|(_, v)| v.as_str());
            fields.push(((*col).to_string(), literal_to_value(lit, ty, col)?));
        }
        out.push(Value::Struct(fields));
    }
    Ok(out)
}

/// Converts a caller-supplied Cypher literal (the legacy pre-UNWIND format:
/// `'foo'` for strings, `10` for ints, `true`/`false` for bools, `1.23` for
/// doubles) into a typed `Value` matching the declared column type.
///
/// A missing literal yields a typed `Value::Null` — lbug accepts NULL in
/// typed columns. Parsing preserves the security guarantees of cypher_str
/// because the string payload is now passed as a typed parameter, not
/// interpolated into Cypher text.
fn literal_to_value(lit: Option<&str>, ty: &LogicalType, col: &str) -> Result<Value, String> {
    let Some(raw) = lit else {
        return Ok(Value::Null(ty.clone()));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null(ty.clone()));
    }
    match ty {
        LogicalType::String => Ok(Value::String(unwrap_cypher_string(trimmed))),
        LogicalType::Int64 => trimmed
            .parse::<i64>()
            .map(Value::Int64)
            .map_err(|e| format!("column {col}: expected INT64, got {raw:?}: {e}")),
        LogicalType::Bool => match trimmed.to_ascii_lowercase().as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" | "" => Ok(Value::Bool(false)),
            _ => Err(format!("column {col}: expected BOOL, got {raw:?}")),
        },
        LogicalType::Double => trimmed
            .parse::<f64>()
            .map(Value::Double)
            .map_err(|e| format!("column {col}: expected DOUBLE, got {raw:?}: {e}")),
        other => Err(format!(
            "column {col}: unsupported bulk-insert type {other:?}"
        )),
    }
}

/// Inverse of `cypher_str`: takes a caller-supplied literal (either an
/// already-quoted Cypher string like `'foo\'s'` or a bare value) and
/// returns the raw string content. The unescape rules mirror cypher_str:
///   \\ → \    \'  → '
/// Any unquoted input is returned as-is (callers sometimes pass raw
/// strings for non-id columns; treat those as literal contents).
fn unwrap_cypher_string(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                match chars.next() {
                    Some('\\') => out.push('\\'),
                    Some('\'') => out.push('\''),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        s.to_string()
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Int64(n) => n.to_string(),
        Value::Int32(n) => n.to_string(),
        Value::Int16(n) => n.to_string(),
        Value::Int8(n) => n.to_string(),
        Value::UInt64(n) => n.to_string(),
        Value::UInt32(n) => n.to_string(),
        Value::UInt16(n) => n.to_string(),
        Value::UInt8(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        _ => format!("{v:?}"),
    }
}

fn value_to_u64(v: &Value) -> u64 {
    match v {
        Value::Int64(n) => *n as u64,
        Value::UInt64(n) => *n,
        _ => 0,
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
