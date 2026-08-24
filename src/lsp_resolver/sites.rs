// lsp_resolver::sites — which call sites the LSP pass must ask about, and
// where the graph's own nodes sit.
//
// Split from `lsp_resolver` when that file crossed the §4.1 500-line cap.
// This half is pure graph reading: it answers "what is still unresolved" and
// "what node lives at this (file, line)". It performs no LSP I/O and inserts
// nothing.

use crate::graph_store::{cypher_str, GraphStore};
use std::collections::HashMap;

/// One call site the static 3b resolver left unresolved, together with
/// everything the LSP round trip needs to ask about it.
pub(super) struct UnresolvedCallSite {
    pub(super) id: String,
    pub(super) caller_qn: String,
    pub(super) caller_label: String,
    #[allow(dead_code)] // retained for diagnostics and future logging
    pub(super) callee_name: String,
    pub(super) file_path: String,
    pub(super) line: u64,
    pub(super) col: u64,
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
    let file_path = extract_file_from_qn(&caller_qn);
    (file_path, caller_qn)
}

pub(super) fn extract_file_from_qn(qn: &str) -> String {
    // fleet-watch#18 adjacent defect (a): a hardcoded .rs/.py/.ts/.tsx list
    // fell through to returning the WHOLE qn for the other seven parsed
    // languages (java/kt/swift/objc/c/cpp/go/rb), producing index keys that
    // can never equal a CallSite file_path. Delegate to the shared
    // all-language helper the static resolver already uses
    // (language_provider::ALL_EXTENSIONS = parser::Language::from_extension).
    crate::language_provider::extract_file_prefix(qn).unwrap_or_else(|| qn.to_string())
}

fn determine_caller_label(store: &GraphStore, caller_qn: &str) -> String {
    let esc = cypher_str(caller_qn);
    for label in &["Function", "Method"] {
        let cypher = format!("MATCH (n:{label}) WHERE n.qualified_name = {esc} RETURN n.id");
        if let Ok(qr) = store.execute_query(&cypher) {
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
            let file = extract_file_from_qn(&row[1]);
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
            assert_eq!(extract_file_from_qn(qn), want, "qn: {qn}");
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

    #[test]
    fn language_id_falls_back_to_plaintext() {
        assert_eq!(language_id_for("rust"), "rust");
        assert_eq!(language_id_for("python"), "python");
        assert_eq!(language_id_for("unknown"), "plaintext");
    }
}
