// search::context — the 360-degree view of one symbol.
//
// Split from `search/mod.rs` when that file crossed the §4.1 cap. `get_context`
// answers a different question from the two ranking paths: not "which symbols
// match this query" but "everything the graph knows about THIS one", so its
// types and its per-relation traversals live apart from the search scorers.

use super::enrichment::parse_opt_u64;
use super::qualified_name::file_path_of;
use super::{resolve_qualified_name, SymbolNotFound, SEARCHABLE_LABELS};
use crate::graph_store::{
    community_of, cypher_str, label_declares_column, process_names, CommunityRow, GraphStore,
    SymbolMatch,
};

pub struct SymbolContext {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
    pub visibility: Option<String>,
    pub imports: Vec<RelatedSymbol>,
    pub imported_by: Vec<RelatedSymbol>,
    pub calls: Vec<RelatedSymbol>,
    pub called_by: Vec<RelatedSymbol>,
    pub implements: Vec<RelatedSymbol>,
    pub implemented_by: Vec<RelatedSymbol>,
    pub uses: Vec<RelatedSymbol>,
    pub used_by: Vec<RelatedSymbol>,
    pub community: Option<CommunityRow>,
    pub processes: Vec<ProcessRef>,
}

pub struct RelatedSymbol {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
}

/// Maximum related symbols collected per direction (calls/called_by/etc.) in
/// `find_related_out` / `find_related_in`.
///
/// source: derived from the response budget. `get_context` ships up to eight
/// such arrays (imports/imported_by/calls/called_by/implements/implemented_by/
/// uses/used_by). With the host cap at `response_budget::MAX_RESPONSE_CHARS`
/// (100_000 chars) and a typical `RelatedSymbol` row serializing to ~80–140
/// chars, dividing the budget across eight sections leaves ~12_500 chars each;
/// at ~140 chars/row that admits ~89 rows. We round to 100 as a clean,
/// slightly-conservative per-direction cap, and push the same value into the
/// Cypher `LIMIT` so the engine never materializes more rows than we keep.
const MAX_RELATED_PER_DIRECTION: usize = 100;

pub struct ProcessRef {
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone)]
pub enum GetContextError {
    NotFound(SymbolNotFound),
    Other(String),
}

impl From<String> for GetContextError {
    fn from(s: String) -> Self {
        GetContextError::Other(s)
    }
}

pub fn get_context(
    store: &GraphStore,
    qualified_name: &str,
) -> Result<SymbolContext, GetContextError> {
    // Layer 1: exact match (qualified_name OR id).
    // Layer 2: strip first path component and retry (src/foo::X → foo::X).
    // Layer 3: name-only fuzzy match — return top candidates.
    let resolved =
        resolve_qualified_name(store, qualified_name).map_err(GetContextError::NotFound)?;

    let escaped = cypher_str(&resolved);
    let (label, name, file_path, start_line, end_line, visibility) =
        find_node_details(store, &escaped)?;

    let imports = find_related_out(store, &escaped, "Imports_");
    let imported_by = find_related_in(store, &escaped, "Imports_");
    let calls = find_related_out(store, &escaped, "Calls_");
    let called_by = find_related_in(store, &escaped, "Calls_");
    let implements = find_related_out(store, &escaped, "Implements_");
    let implemented_by = find_related_in(store, &escaped, "Implements_");
    let uses = find_related_out(store, &escaped, "Uses_");
    let used_by = find_related_in(store, &escaped, "Uses_");
    let community = find_community(store, &resolved);
    let processes = find_processes(store, &resolved);

    Ok(SymbolContext {
        qualified_name: resolved,
        name,
        label,
        file_path,
        start_line,
        end_line,
        visibility,
        imports,
        imported_by,
        calls,
        called_by,
        implements,
        implemented_by,
        uses,
        used_by,
        community,
        processes,
    })
}

/// (label, name, file_path, start_line, end_line, visibility) for a resolved
/// symbol node. `Module`/`Constant`/`TypeAlias` labels have no line range,
/// so start_line/end_line/visibility are None for those. clippy::type_complexity.
type NodeDetails = (
    String,
    String,
    String,
    Option<u64>,
    Option<u64>,
    Option<String>,
);

fn find_node_details(store: &GraphStore, escaped: &str) -> Result<NodeDetails, String> {
    for &label in SEARCHABLE_LABELS {
        let has_lines = label_declares_column(label, "start_line");
        let has_visibility = label_declares_column(label, "visibility");
        let mut columns = String::from("n.name, n.qualified_name");
        if has_lines {
            columns.push_str(", n.start_line, n.end_line");
        }
        if has_visibility {
            columns.push_str(", n.visibility");
        }
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = {escaped} OR n.id = {escaped} \
             RETURN {columns}"
        );
        let Ok(qr) = store.execute_query(&cypher) else {
            continue;
        };
        let Some(row) = qr.rows.first() else {
            continue;
        };
        if row.len() < 2 {
            continue;
        }
        let (start_line, end_line) = if has_lines {
            (
                row.get(2).and_then(|v| parse_opt_u64(v)),
                row.get(3).and_then(|v| parse_opt_u64(v)),
            )
        } else {
            (None, None)
        };
        // Visibility trails the optional line columns, so its index depends on
        // whether they were requested.
        let visibility_at = if has_lines { 4 } else { 2 };
        let visibility = if has_visibility {
            row.get(visibility_at).cloned()
        } else {
            None
        };
        return Ok((
            label.to_string(),
            row[0].clone(),
            file_path_of(&row[1]).to_string(),
            start_line,
            end_line,
            visibility,
        ));
    }
    Err(format!("symbol not found: {escaped}"))
}

fn find_related_out(store: &GraphStore, escaped: &str, prefix: &str) -> Vec<RelatedSymbol> {
    let mut related = Vec::new();
    for &(rel, from_label, to_label) in crate::graph_store::REL_TABLES {
        if related.len() >= MAX_RELATED_PER_DIRECTION {
            break;
        }
        if !rel.starts_with(prefix) {
            continue;
        }
        // source: stages/stage-3b-v2.md §5 — StdlibSymbol targets are
        // infrastructure-only (used for analysis + query_graph precision
        // metrics) and explicitly excluded from the flat get_context
        // calls/called_by result. Agents asking "what does X call?" expect
        // user-code callees, not every framework/std method implicitly
        // invoked.
        if to_label == crate::graph_store::NODE_STDLIB_SYMBOL {
            continue;
        }
        // LIMIT bounds the per-relation result so an unbounded fan-out cannot
        // flood the accumulated Vec or the downstream MCP response.
        let cypher = format!(
            "MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) \
             WHERE a.qualified_name = {escaped} OR a.id = {escaped} \
             RETURN b.name, b.qualified_name LIMIT {MAX_RELATED_PER_DIRECTION}"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if related.len() >= MAX_RELATED_PER_DIRECTION {
                    break;
                }
                if row.len() >= 2 {
                    related.push(RelatedSymbol {
                        name: row[0].clone(),
                        qualified_name: row[1].clone(),
                        label: to_label.to_string(),
                    });
                }
            }
        }
    }
    related
}

fn find_related_in(store: &GraphStore, escaped: &str, prefix: &str) -> Vec<RelatedSymbol> {
    let mut related = Vec::new();
    for &(rel, from_label, to_label) in crate::graph_store::REL_TABLES {
        if related.len() >= MAX_RELATED_PER_DIRECTION {
            break;
        }
        if !rel.starts_with(prefix) {
            continue;
        }
        // source: see find_related_out — symmetric exclusion.
        if to_label == crate::graph_store::NODE_STDLIB_SYMBOL {
            continue;
        }
        // LIMIT bounds the per-relation result; see find_related_out.
        let cypher = format!(
            "MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) \
             WHERE b.qualified_name = {escaped} OR b.id = {escaped} \
             RETURN a.name, a.qualified_name LIMIT {MAX_RELATED_PER_DIRECTION}"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if related.len() >= MAX_RELATED_PER_DIRECTION {
                    break;
                }
                if row.len() >= 2 {
                    related.push(RelatedSymbol {
                        name: row[0].clone(),
                        qualified_name: row[1].clone(),
                        label: from_label.to_string(),
                    });
                }
            }
        }
    }
    related
}

/// The community the target belongs to, through the shared membership
/// traversal. Takes the RAW target — `membership` owns the escaping.
fn find_community(store: &GraphStore, target: &str) -> Option<CommunityRow> {
    SEARCHABLE_LABELS
        .iter()
        .find_map(|label| community_of(store, label, SymbolMatch::IdOrQualifiedName(target)))
}

/// The processes the target takes part in, tagged by role.
///
/// The participant half is the shared `ParticipatesIn` traversal; only the
/// entry-point half is spelled out here, because `EntryPointOf` is this
/// module's own question and has no second copy to share with. Order is
/// per label: every entry-point row first, then participant rows that are not
/// already listed.
fn find_processes(store: &GraphStore, target: &str) -> Vec<ProcessRef> {
    let symbol = SymbolMatch::IdOrQualifiedName(target);
    let escaped = cypher_str(target);
    let mut procs: Vec<ProcessRef> = Vec::new();

    for label in ["Function", "Method"] {
        for name in entry_point_processes(store, label, &escaped) {
            procs.push(ProcessRef {
                name,
                role: "entry_point".to_string(),
            });
        }
        for name in process_names(store, label, symbol) {
            if !procs.iter().any(|pr| pr.name == name) {
                procs.push(ProcessRef {
                    name,
                    role: "participant".to_string(),
                });
            }
        }
    }
    procs
}

/// Process names the target is the declared entry point of, under `label`.
/// `escaped` is an already-quoted Cypher literal built by the caller.
fn entry_point_processes(store: &GraphStore, label: &str, escaped: &str) -> Vec<String> {
    let rel = format!("EntryPointOf_{label}_Process");
    let cypher = format!(
        "MATCH (n:{label})-[:{rel}]->(p:Process) \
         WHERE n.qualified_name = {escaped} OR n.id = {escaped} \
         RETURN p.name"
    );
    match store.execute_query(&cypher) {
        Ok(qr) => qr
            .rows
            .iter()
            .filter_map(|row| row.first())
            // An empty `Process.name` names no process — the same rule
            // `membership::first_column` applies to the ParticipatesIn half of
            // this function. `EntryPointOf` has no second copy to share, so the
            // rule is restated here rather than routed.
            .filter(|name| !name.is_empty())
            .cloned()
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Integration tests — build a real graph from src/ and exercise the
    // three-layer qualified-name lookup end to end.
    use crate::graph_store::GraphStore;
    use crate::indexer::index_codebase;
    use std::path::Path;

    fn fresh_store(tag: &str) -> (crate::test_support::TestTempDir, GraphStore) {
        use crate::test_support::TempDirExt;
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let tmp = tempfile::Builder::new()
            .prefix(&format!("search_test_{tag}_"))
            .tempdir()
            .expect("create temp dir")
            .keep_managed();
        let _ = std::fs::remove_dir_all(&tmp);
        let r = index_codebase(Path::new("src"), &tmp).unwrap();
        let store = GraphStore::open_or_create(&r.graph_path).unwrap();
        (tmp, store)
    }

    #[test]
    fn test_get_context_accepts_src_prefix() {
        // Callers passing `src/main.rs::handle_tool_call` must resolve to the
        // stored `main.rs::handle_tool_call`.
        let (tmp, store) = fresh_store("src_prefix");
        let resolved = resolve_qualified_name(&store, "src/main.rs::handle_tool_call")
            .expect("src/-prefixed qualified_name must resolve");
        assert_eq!(resolved, "main.rs::handle_tool_call");

        let ctx = get_context(&store, "src/main.rs::handle_tool_call")
            .map_err(|e| match e {
                GetContextError::NotFound(nf) => format!("not found: {:?}", nf.did_you_mean),
                GetContextError::Other(s) => s,
            })
            .expect("get_context must succeed with src/ prefix");
        assert_eq!(ctx.qualified_name, "main.rs::handle_tool_call");
        assert_eq!(ctx.name, "handle_tool_call");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_get_context_did_you_mean() {
        // An unknown qualified_name must fail with suggestions, not a
        // flat string error.
        let (tmp, store) = fresh_store("did_you_mean");
        let err = get_context(&store, "nonexistent.rs::handle_tool_call")
            .err()
            .expect("unknown qn must error");
        match err {
            GetContextError::NotFound(nf) => {
                assert_eq!(nf.input, "nonexistent.rs::handle_tool_call");
                assert!(
                    !nf.did_you_mean.is_empty(),
                    "did_you_mean must include candidates by name"
                );
                assert!(
                    nf.did_you_mean
                        .iter()
                        .any(|s| s.ends_with("::handle_tool_call")),
                    "expected a `handle_tool_call` candidate, got {:?}",
                    nf.did_you_mean
                );
            }
            GetContextError::Other(m) => panic!("expected NotFound, got Other({m})"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Round-5 finding 3. `find_processes`' entry-point half read `p.name`
    /// directly and forwarded an empty one, so `get_context` could report a
    /// process that names nothing while every other consumer of the same graph
    /// reported none. Third sibling of this defect class in three rounds.
    #[test]
    fn an_empty_process_name_is_not_an_entry_point() {
        use crate::graph_store::{GraphStore, NODE_FUNCTION, NODE_PROCESS};

        let dir = tempfile::Builder::new()
            .prefix("context_empty_process_name")
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");

        let qn = "m.rs::f";
        store
            .insert_node(NODE_PROCESS, &[("id", "'p0'"), ("name", "''")])
            .expect("process");
        store
            .insert_node(
                NODE_FUNCTION,
                &[
                    ("id", "'m.rs::f'"),
                    ("name", "'f'"),
                    ("qualified_name", "'m.rs::f'"),
                    ("start_line", "1"),
                    ("end_line", "1"),
                    ("visibility", "'pub'"),
                    ("is_async", "false"),
                ],
            )
            .expect("fn");
        store
            .insert_edge("EntryPointOf_Function_Process", qn, "p0", &[])
            .expect("EntryPointOf");

        // Precondition: the degenerate edge really exists.
        assert_eq!(
            store
                .execute_query(
                    "MATCH (n:Function)-[:EntryPointOf_Function_Process]->(p:Process) \
                     RETURN p.id"
                )
                .expect("probe")
                .rows
                .len(),
            1
        );
        assert!(
            find_processes(&store, qn).is_empty(),
            "an empty Process.name names no process, on the entry-point half too"
        );
    }
}
