// lsp_resolver::sites — which call sites the LSP pass must ask about, and
// where the graph's own nodes sit.
//
// Split from `lsp_resolver` when that file crossed the §4.1 500-line cap.
// This half is pure graph reading: it answers "what is still unresolved" and
// "what node lives at this (file, line)". It performs no LSP I/O and inserts
// nothing.

use crate::graph_store::GraphStore;
use crate::language_provider::extract_file_prefix_or_self;
use lbug::Value;
use std::collections::HashMap;

/// One call site the static 3b resolver left unresolved, together with
/// everything the LSP round trip needs to ask about it.
pub(super) struct UnresolvedCallSite {
    pub(super) id: String,
    pub(super) caller_qn: String,
    pub(super) caller_label: String,
    pub(super) callee_name: String,
    pub(super) file_path: String,
    pub(super) line: u64,
    pub(super) col: u64,
}

impl UnresolvedCallSite {
    /// The 0-based (line, character) `textDocument/definition` must target:
    /// the call's own identifier, never its receiver.
    ///
    /// Two corrections over the stored (line, col):
    ///
    /// - `line` is 1-based in the graph (`CallSite.line` == the parser's
    ///   `node.start_line`); LSP positions are 0-based (LSP 3.17 §Text
    ///   Documents).
    /// - `col` (as of the `lsp_col` property fix in
    ///   `indexer::persist::nodes`) is 0-based but still points at the START
    ///   of the callee expression — the RECEIVER for a method call, not the
    ///   method. Verified 2026-09-03 against rust-analyzer on
    ///   `self.response_of(i)`: querying column 16 (`self`) returns the
    ///   `self` binding; column 21 (`response_of`) returns the method.
    ///   `last_segment_offset` walks to the byte offset of the LAST `.` or
    ///   `::` in `callee_name` — which is the verbatim source substring
    ///   starting at `col` (every `call_callee`/`call_entry` in
    ///   `src/parser/spec/*.rs` reads it straight from the source file), so
    ///   that offset is exactly the identifier's column.
    pub(super) fn lsp_position(&self) -> (u64, u64) {
        let line0 = self.line.saturating_sub(1);
        let col = self.col + last_segment_offset(&self.callee_name) as u64;
        (line0, col)
    }

    /// The identifier text this call site is actually asking to resolve —
    /// the same last `.`/`::`-separated segment `lsp_position` points at,
    /// e.g. `self.response_of` -> `"response_of"`, a bare `helper` ->
    /// `"helper"`.
    ///
    /// Used by `edges::try_add_lsp_edge` to refuse a resolved definition
    /// whose OWN name does not match this identifier — the defense against
    /// a same-line collision `find_node_at_position` cannot rule out on
    /// line-only data (fabricated `total -> total` self-edge, PR #267
    /// follow-up: `extra_call_entries` (#87) emits a speculative CallSite
    /// for a bare-identifier argument; rust-analyzer correctly resolves it
    /// to its own PARAMETER declaration, which sits on the same line as the
    /// enclosing method's own declaration, and a parameter is not itself an
    /// indexed graph node).
    pub(super) fn identifier_name(&self) -> &str {
        &self.callee_name[last_segment_offset(&self.callee_name)..]
    }
}

/// Byte offset, within `callee_name`, of the start of its LAST `.`- or
/// `::`-separated segment: the actual identifier a receiver/path expression
/// resolves through. `self.response_of` -> offset of `response_of`;
/// `helpers::normalize` -> offset of `normalize`; a bare name (no separator)
/// is its own last segment, offset 0.
///
/// Byte offsets, not char offsets: LSP `character` is a UTF-16 code unit
/// count (LSP 3.17 §Text Documents), and tree-sitter's own `column` is a byte
/// count — the two coincide only for ASCII source text. Every column already
/// flowing through this pass (`lsp_col`, `CallSite.col`) carries the same
/// ASCII assumption; widening it to non-ASCII identifiers is out of scope
/// here and would need re-encoding at the parser, not at this call site.
fn last_segment_offset(callee_name: &str) -> usize {
    let dot = callee_name.rfind('.').map(|i| i + 1);
    let scope = callee_name.rfind("::").map(|i| i + 2);
    dot.into_iter().chain(scope).max().unwrap_or(0)
}

/// Every call site the static 3b resolver left open.
///
/// fleet-watch#18 adjacent defect (b): "unresolved" was previously decided at
/// CALLER granularity — "does the enclosing function have ANY outgoing Calls
/// edge" — so a caller with ten sites and one static resolution had its nine
/// remaining sites skipped. The per-site marker the static resolver actually
/// maintains is `CallSite.is_resolved` (§10.4, flipped by `resolver::calls`
/// via `mark_nodes_resolved`); this filters on it directly.
///
/// Review finding 4 — two ways a graph indexed by an older build defeats that
/// filter, both measured 2026-08-24 on lbug 0.19.1:
///
///   * the COLUMN may be absent, and referencing a missing property is a hard
///     binder error, not an empty result — it took the whole tool down.
///     `ensure_node_column` adds it (with a DEFAULT, which backfills the
///     existing rows) and is a no-op once present.
///   * the VALUE may be NULL, from an indexer that predates §10.4's "the
///     indexer writes false". Under three-valued logic `NULL = false` is NULL,
///     so those rows were filtered OUT — the pass collected nothing and
///     reported a successful zero-site run. A never-attempted site is
///     unresolved, which is exactly what `IS NULL` says here.
pub(super) fn collect_unresolved_callsites(
    store: &GraphStore,
) -> Result<Vec<UnresolvedCallSite>, String> {
    store.ensure_node_column("CallSite", "is_resolved", "BOOLEAN DEFAULT false")?;
    let qr = store.execute_query(
        "MATCH (cs:CallSite) WHERE cs.is_resolved IS NULL OR cs.is_resolved = false \
         RETURN cs.id, cs.callee_name, cs.line, cs.col",
    )?;

    let mut sites = Vec::new();
    for row in &qr.rows {
        if row.len() < 4 {
            continue;
        }
        let cs_id = &row[0];
        let callee = &row[1];
        let line: u64 = row[2].parse().unwrap_or(0);
        let col: u64 = row[3].parse().unwrap_or(0);

        let (file_path, caller_qn) = parse_callsite_id(cs_id);
        let caller_label = determine_caller_label(store, &caller_qn);

        sites.push(UnresolvedCallSite {
            id: cs_id.clone(),
            caller_qn,
            caller_label,
            callee_name: callee.clone(),
            file_path,
            line,
            col,
        });
    }
    Ok(sites)
}

fn extract_caller_from_callsite_id(cs_id: &str) -> String {
    if let Some(idx) = cs_id.rfind("::call@") {
        cs_id[..idx].to_string()
    } else {
        cs_id.to_string()
    }
}

fn parse_callsite_id(cs_id: &str) -> (String, String) {
    let caller_qn = extract_caller_from_callsite_id(cs_id);
    let file_path = extract_file_prefix_or_self(&caller_qn);
    (file_path, caller_qn)
}

fn determine_caller_label(store: &GraphStore, caller_qn: &str) -> String {
    for label in &["Function", "Method"] {
        // Bound, not interpolated: `caller_qn` is derived from an indexed file
        // path. The text is constant per label, so the prepared statement is
        // cached across every call site in the run.
        let cypher = format!("MATCH (n:{label}) WHERE n.qualified_name = $v RETURN n.id");
        let params = vec![("v", Value::String(caller_qn.to_string()))];
        if let Ok(qr) = store.query_prepared_params(&cypher, params) {
            if !qr.rows.is_empty() {
                return label.to_string();
            }
        }
    }
    "Function".to_string()
}

pub(super) fn group_by_file(
    sites: &[UnresolvedCallSite],
) -> HashMap<String, Vec<&UnresolvedCallSite>> {
    let mut map: HashMap<String, Vec<&UnresolvedCallSite>> = HashMap::new();
    for site in sites {
        map.entry(site.file_path.clone()).or_default().push(site);
    }
    map
}

pub(super) fn language_id_for(language: &str) -> &str {
    match language {
        "rust" => "rust",
        "python" => "python",
        "typescript" => "typescript",
        _ => "plaintext",
    }
}

/// A graph node's identity at a (file, start_line) position.
pub(super) struct NodePosition {
    pub(super) id: String,
    pub(super) label: String,
}

pub(super) fn build_node_position_index(
    store: &GraphStore,
) -> Result<HashMap<(String, u64), NodePosition>, String> {
    let mut index = HashMap::new();
    for label in &["Function", "Method", "Struct", "Enum", "Trait"] {
        let qr = store.execute_query(&format!(
            "MATCH (n:{label}) RETURN n.id, n.qualified_name, n.start_line"
        ))?;
        for row in &qr.rows {
            if row.len() < 3 {
                continue;
            }
            let file = extract_file_prefix_or_self(&row[1]);
            let line: u64 = row[2].parse().unwrap_or(0);
            index.insert(
                (file, line),
                NodePosition {
                    id: row[0].clone(),
                    label: label.to_string(),
                },
            );
        }
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_store::NODE_CALL_SITE;

    fn store_with_schema(prefix: &str) -> (tempfile::TempDir, GraphStore) {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");
        (dir, store)
    }

    fn insert_site(store: &GraphStore, id: &str, resolved: Option<&str>) {
        let mut props = vec![
            ("id", format!("'{id}'")),
            ("callee_name", "'x'".to_string()),
            ("line", "5".to_string()),
            ("col", "4".to_string()),
            ("language", "'rust'".to_string()),
        ];
        if let Some(v) = resolved {
            props.push(("is_resolved", v.to_string()));
        }
        let borrowed: Vec<(&str, &str)> = props.iter().map(|(k, v)| (*k, v.as_str())).collect();
        store
            .insert_node(NODE_CALL_SITE, &borrowed)
            .unwrap_or_else(|e| panic!("insert {id}: {e}"));
    }

    #[test]
    fn extract_file_from_qn_covers_every_parsed_language() {
        // fleet-watch#18 adjacent defect (a): the old hardcoded list only
        // knew .rs/.py/.ts/.tsx and returned the whole qn for the rest.
        for (qn, want) in [
            ("src/main.rs::main", "src/main.rs"),
            ("app/Main.java::Main::run", "app/Main.java"),
            ("pkg/server.go::Handle", "pkg/server.go"),
            ("src/App.kt::App", "src/App.kt"),
            ("lib/util.cpp::frob", "lib/util.cpp"),
            ("lib/tool.rb::frob", "lib/tool.rb"),
            ("ui/View.swift::View::render", "ui/View.swift"),
        ] {
            assert_eq!(extract_file_prefix_or_self(qn), want, "qn: {qn}");
        }
    }

    #[test]
    fn collect_skips_resolved_sites_but_keeps_unresolved_siblings() {
        // fleet-watch#18 adjacent defect (b): "unresolved" was decided at
        // caller granularity, so one statically resolved site hid every
        // sibling site of the same caller. The per-site `is_resolved` marker
        // (§10.4) is the signal the static resolver actually maintains.
        let (_dir, store) = store_with_schema("lsp_collect_test");
        insert_site(&store, "src/a.rs::caller::call@5:4", Some("true"));
        insert_site(&store, "src/a.rs::caller::call@7:4", Some("false"));

        let sites = collect_unresolved_callsites(&store).expect("collect");
        assert_eq!(
            sites.len(),
            1,
            "only the is_resolved=false sibling must be collected"
        );
        assert_eq!(sites[0].id, "src/a.rs::caller::call@7:4");
        assert_eq!(sites[0].caller_qn, "src/a.rs::caller");
        assert_eq!(sites[0].file_path, "src/a.rs");
    }

    #[test]
    fn a_null_is_resolved_counts_as_unresolved() {
        // Review finding 4 (regression, second half). A site written by an
        // indexer that predates §10.4's "the indexer writes false" carries
        // NULL. Under three-valued logic `NULL = false` is NULL, so `WHERE
        // cs.is_resolved = false` filtered those rows OUT and the pass
        // reported a successful run over zero sites — a silent no-op on
        // exactly the graphs that needed it most.
        let (_dir, store) = store_with_schema("lsp_null_resolved");
        insert_site(&store, "src/a.rs::caller::call@9:2", None);
        let sites = collect_unresolved_callsites(&store).expect("collect");
        assert_eq!(
            sites.len(),
            1,
            "a never-attempted (NULL) site is unresolved and must be collected"
        );
    }

    #[test]
    fn a_graph_without_the_is_resolved_column_is_migrated_not_rejected() {
        // Review finding 4 (regression, first half). Referencing a property
        // the table does not carry is a hard binder error, not an empty
        // result (measured 2026-08-24, lbug 0.19.1) — so on a graph indexed
        // before the column existed the filter took the whole tool down.
        let dir = tempfile::Builder::new()
            .prefix("lsp_pre_is_resolved")
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        // A CallSite table as an older build wrote it: no `is_resolved`.
        store
            .execute_query(
                "CREATE NODE TABLE CallSite(id STRING, callee_name STRING, \
                 line INT64, col INT64, language STRING, PRIMARY KEY(id))",
            )
            .expect("legacy CallSite table");
        insert_site(&store, "src/a.rs::caller::call@3:1", None);

        let sites = collect_unresolved_callsites(&store)
            .expect("a pre-is_resolved graph must be migrated, not rejected");
        assert_eq!(sites.len(), 1, "the legacy site must be collected");
        // The migration ran once and is idempotent.
        assert!(!store
            .ensure_node_column("CallSite", "is_resolved", "BOOLEAN DEFAULT false")
            .expect("second call"));
    }

    #[test]
    fn extract_caller_from_a_callsite_id_drops_the_call_suffix() {
        assert_eq!(
            extract_caller_from_callsite_id("src/main.rs::main::call@5:4"),
            "src/main.rs::main"
        );
        assert_eq!(
            extract_caller_from_callsite_id("src/foo.rs::bar"),
            "src/foo.rs::bar"
        );
    }

    /// Root cause 2 (fix/lsp-receiver-calls). `CallSite.col` (post the
    /// `lsp_col` fix) still points at the START of the callee expression —
    /// the RECEIVER for a method call — not the method identifier.
    /// rust-analyzer resolves that position to the receiver's own binding,
    /// not the method: verified 2026-09-03 on `self.response_of(i)`, column
    /// 16 (`self`) -> the `self` binding, column 21 (`response_of`) -> the
    /// method, same line. `lsp_position` must therefore target the LAST
    /// `.`/`::`-separated segment, never the stored column verbatim.
    #[test]
    fn lsp_definition_targets_method_identifier_not_receiver() {
        let cases: &[(&str, u64, u64, u64)] = &[
            // (callee_name, line, col, expected identifier col)
            ("self.response_of", 5, 8, 8 + "self.".len() as u64),
            ("s.response_of", 5, 8, 8 + "s.".len() as u64),
            ("trial.response_of", 5, 8, 8 + "trial.".len() as u64),
            ("helpers::normalize", 5, 8, 8 + "helpers::".len() as u64),
            // A chained call: the LAST segment is the identifier, not the
            // first receiver nor an intermediate call's parens.
            (
                "input.trim().to_string",
                5,
                8,
                8 + "input.trim().".len() as u64,
            ),
            // A bare function call has no separator: identifier == col.
            ("helper", 5, 8, 8),
        ];
        for (callee_name, line, col, expected_col) in cases {
            let site = UnresolvedCallSite {
                id: "src/a.rs::caller::call@x".to_string(),
                caller_qn: "src/a.rs::caller".to_string(),
                caller_label: "Method".to_string(),
                callee_name: callee_name.to_string(),
                file_path: "src/a.rs".to_string(),
                line: *line,
                col: *col,
            };
            let (lsp_line, lsp_col) = site.lsp_position();
            assert_eq!(
                lsp_line,
                line - 1,
                "line must be converted from the graph's 1-based to LSP's \
                 0-based: {callee_name}"
            );
            assert_eq!(
                lsp_col, *expected_col,
                "identifier column for callee_name={callee_name:?}"
            );
        }
    }

    #[test]
    fn language_id_falls_back_to_plaintext() {
        assert_eq!(language_id_for("rust"), "rust");
        assert_eq!(language_id_for("python"), "python");
        assert_eq!(language_id_for("unknown"), "plaintext");
    }
}
