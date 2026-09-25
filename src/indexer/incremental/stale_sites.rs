// indexer::incremental::stale_sites: the call sites whose resolution a purge
// takes away (issue #353, review rounds 2 and 3).
//
// An incremental refresh purges the nodes of a changed, deleted or renamed file.
// A call site of ANOTHER file that had a per-site resolution edge
// (`Calls_CallSite_*`) into one of those nodes keeps its `is_resolved = true`
// flag while the edge dies with the node, so the graph would claim a resolution
// it no longer has. The purge is the source of truth here, not the absence of an
// edge: a site with no per-site edge can be resolved for a legitimate reason (a
// macro site, or a language-server site that points at a Trait or an Enum) and
// must keep its flag. So only the sites that HAD an edge into a purged node are
// touched.
//
// Which tiers write a per-site row: the static resolver and the language server
// pass write `Calls_CallSite_Function|Method` beside the symbol-level edge (the
// macro tier writes `Calls_CallSite_StdlibSymbol`, and a call that names a type
// writes `Calls_CallSite_Struct`, issue #356). The table list comes from
// `REL_TABLES`, so a per-site table added there is collected without a change here.
//
// Known gap, accepted: a site resolved WITHOUT a per-site row (a macro site, a
// language-server site that points at a Trait or an Enum) whose target file is
// DELETED keeps `is_resolved = true`, because the purge sees no edge to reopen.
// The constructor case of this gap is closed by `Calls_CallSite_Struct`.
//
// Order and crash safety. `reopen_sites_resolved_into` runs BEFORE the purge and
// `restore_sites_with_edge` AFTER the relink, each idempotent:
//   1. collect the sites with an edge into a purged file and set them to `false`
//      (and clear their reason): from this point no site claims a resolution the
//      purge may take away;
//   2. purge, reparse, relink (the pre-existing steps);
//   3. set back to `true` the collected sites that hold a per-site edge again
//      (their target survived the rewrite and the edge was relinked).
// A crash after 1 leaves a site `false` while an edge may still exist: the safe
// side (an under-claim the next resolve or language-server pass repairs). A crash
// before 1 changed nothing. Re-running the refresh after a crash re-collects
// whatever edges still exist and repeats the same idempotent statements. The
// reverse order (purge first, collect after) would lose the information for good,
// because the edges are gone once the nodes are.

use crate::graph_store::{cypher_str, GraphStore, REL_TABLES};
use std::collections::HashSet;

/// Sites flagged open before a purge of `purged_rels`: those OUTSIDE `skip` (the
/// purged and reparsed files, whose sites are deleted or recreated) that hold a
/// per-site call edge into a symbol of a purged file. Returns their ids.
pub(super) fn reopen_sites_resolved_into(
    store: &GraphStore,
    purged_rels: &[&str],
    skip: &HashSet<String>,
) -> Result<Vec<String>, String> {
    if purged_rels.is_empty() {
        return Ok(Vec::new());
    }
    let target = purged_rels
        .iter()
        .map(|rel| format!("starts_with(t.id, {})", cypher_str(&format!("{rel}::"))))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut ids: HashSet<String> = HashSet::new();
    for table in call_site_tables() {
        let rows = store.execute_query(&format!(
            "MATCH (c:CallSite)-[r:{table}]->(t) WHERE {target} RETURN c.id"
        ))?;
        for row in rows.rows {
            let owner = row[0].split("::").next().unwrap_or("");
            if !skip.contains(owner) {
                ids.insert(row[0].clone());
            }
        }
    }
    let mut ids: Vec<String> = ids.into_iter().collect();
    ids.sort();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    store.mark_callsites_unresolved(&refs)?;
    if !refs.is_empty() {
        store.ensure_node_column("CallSite", "unresolved_reason", "STRING DEFAULT ''")?;
        store.set_callsite_unresolved_reason(&refs, "")?;
    }
    Ok(ids)
}

/// Sets back to `true` the sites of `ids` that hold a per-site call edge again.
pub(super) fn restore_sites_with_edge(store: &GraphStore, ids: &[String]) -> Result<(), String> {
    let mut with_edge: HashSet<String> = HashSet::new();
    for chunk in ids.chunks(500) {
        let list = chunk
            .iter()
            .map(|id| cypher_str(id))
            .collect::<Vec<_>>()
            .join(", ");
        for table in call_site_tables() {
            let rows = store.execute_query(&format!(
                "MATCH (c:CallSite)-[r:{table}]->() WHERE c.id IN [{list}] RETURN c.id"
            ))?;
            with_edge.extend(rows.rows.into_iter().map(|row| row[0].clone()));
        }
    }
    let mut back: Vec<&str> = with_edge.iter().map(String::as_str).collect();
    back.sort_unstable();
    store.mark_nodes_resolved("CallSite", &back)
}

fn call_site_tables() -> impl Iterator<Item = &'static str> {
    REL_TABLES
        .iter()
        .filter(|(name, from, _)| *from == "CallSite" && name.starts_with("Calls_"))
        .map(|(name, _, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{indexer, resolver};

    fn flag_and_rows(store: &GraphStore) -> (String, usize) {
        let flag = store
            .execute_query("MATCH (c:CallSite) WHERE c.callee_name = 'helper' RETURN c.is_resolved")
            .unwrap()
            .rows[0][0]
            .to_lowercase();
        let rows = store
            .execute_query("MATCH (c:CallSite)-[r:Calls_CallSite_Function]->() RETURN c.id")
            .unwrap()
            .rows
            .len();
        (flag, rows)
    }

    /// Each step is idempotent, so a refresh replayed after a crash between the
    /// steps converges: running the sequence twice gives the state of running it
    /// once, whether the edge survived or not.
    #[test]
    fn the_sequence_run_twice_gives_the_state_of_running_it_once() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("a.rs"), "pub fn helper() {}\n").unwrap();
        std::fs::write(source.join("b.rs"), "fn f() {\n    helper();\n}\n").unwrap();
        let graph = tmp.path().join("graph");
        indexer::index_codebase(&source, &graph).unwrap();
        let store = GraphStore::open_or_create(&graph).unwrap();
        resolver::resolve_graph(&store).unwrap();
        assert_eq!(flag_and_rows(&store), ("true".to_string(), 1), "control");

        let skip: HashSet<String> = ["a.rs".to_string()].into();
        let first = reopen_sites_resolved_into(&store, &["a.rs"], &skip).unwrap();
        let after_first = flag_and_rows(&store);
        let second = reopen_sites_resolved_into(&store, &["a.rs"], &skip).unwrap();
        assert_eq!(first, second, "the same sites are collected again");
        assert_eq!(flag_and_rows(&store), after_first);
        assert_eq!(after_first.0, "false");

        // The edge is still there (no purge happened): restoring twice is the
        // same as once, and the site is resolved again.
        restore_sites_with_edge(&store, &first).unwrap();
        let restored = flag_and_rows(&store);
        restore_sites_with_edge(&store, &first).unwrap();
        assert_eq!(flag_and_rows(&store), restored);
        assert_eq!(restored, ("true".to_string(), 1));
    }
}
