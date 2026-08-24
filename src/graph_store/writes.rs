// graph_store::writes — the write-side surface of `GraphStore`.
//
// Moved verbatim from mod.rs (Fowler "Move Function", no behavior change) to
// keep each `impl GraphStore` block within the §4.3 size cap along a concern
// boundary: this file owns every method that MUTATES the graph (single and
// bulk inserts, resolution-state flips); mod.rs keeps lifecycle and the
// read/query surface.

use super::columns::{edge_column_types, node_column_types};
use super::ddl::parse_rel_endpoints;
use super::serialize::{
    build_edge_struct_rows, build_edge_unwind, build_node_unwind, build_struct_rows,
    edge_prop_order, format_props, node_prop_order,
};
use super::{cypher_str, GraphStore, PropEdge, BULK_BATCH_SIZE};
use lbug::{LogicalType, Value};

impl GraphStore {
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

    /// Inserts one `FileContent` row: the file's zstd-compressed source
    /// bytes, keyed by the file's relative path (matches `File.id`).
    ///
    /// Bypasses the Cypher-string-literal path (`insert_node`/`cypher_str`)
    /// entirely and the bulk UNWIND path (`bulk_insert_nodes`) — both go
    /// through `literal_to_value`, which parses UTF-8 text into typed
    /// values; compressed bytes are not valid UTF-8 and cannot be safely
    /// represented as a Cypher string literal. This builds a typed
    /// `Value::Blob` directly and binds it via a prepared statement with
    /// named parameters, the same FFI path `bulk_insert_nodes` uses for its
    /// struct rows, just without the UNWIND/List wrapper (one row per call).
    ///
    /// Cached like every other prepared statement on this store (see
    /// `run_prepared`): the cypher text is identical on every call (only the
    /// parameter VALUES differ per file), so caching turns N calls into one
    /// plan + N binds instead of N plans.
    pub(crate) fn insert_file_content(
        &self,
        file_id: &str,
        content_zstd: Vec<u8>,
        original_size: i64,
    ) -> Result<(), String> {
        let compressed_size = content_zstd.len() as i64;
        let cypher = "CREATE (:FileContent {id: $id, content_zstd: $content, \
                       original_size: $original_size, compressed_size: $compressed_size})";
        let params = vec![
            ("id", Value::String(file_id.to_string())),
            ("content", Value::Blob(content_zstd)),
            ("original_size", Value::Int64(original_size)),
            ("compressed_size", Value::Int64(compressed_size)),
        ];
        self.run_prepared_params(cypher, params)
    }

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
}
