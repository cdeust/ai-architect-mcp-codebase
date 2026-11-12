// indexer::incremental::cross_links: the file-to-file links an incremental pass
// can leave stale in a file it does not re-parse (Codex review of #523).
//
// Two post-passes of the index draw edges between files: the light links
// (`light_link`: JS imports, Markdown and shell references) and the IaC
// references (`iac`: image to Dockerfile, ConfigMap/Secret/Service by name,
// COPY source, kustomize paths). An incremental pass re-runs both for the files
// it re-parses only, so an unchanged source keeps the edges an earlier tree gave
// it:
//   - light links resolve against the set of File ids. Adding `util.js` gives
//     an unchanged `a.js` (`import "./util"`) the `Imports_File_File` a fresh
//     index draws; deleting `util.js` can move it to `util/index.js`; an added
//     second `x.sh` makes a `$DIR/x.sh` reference ambiguous, so it drops. They
//     do not read the target's content, so only a changed file SET matters,
//     and the cure is to draw again the links of every unchanged source
//     (`relink_unchanged_sources`): one read per Markdown, JS or shell file,
//     far less than a rebuild.
//   - IaC references also resolve by name against other files' IaC nodes:
//     renaming only a ConfigMap's `metadata.name` leaves the unchanged
//     Deployment's `Imports_IacResource_File`. Drawing them again would mean
//     re-emitting every manifest's nodes; until that invalidation exists, a
//     pass that may have moved one says so (`iac_references_stale`) and the
//     caller rebuilds.

use super::mutate::purge_outbound_light_links;
use super::{ChangeSet, Discovered};
use crate::graph_store::{GraphStore, NODE_IAC_MODULE, NODE_IAC_RESOURCE};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// True when the change set adds, deletes or renames a file: the set of File
/// ids every light link and IaC reference resolves against moved.
pub(super) fn file_set_changed(changes: &ChangeSet) -> bool {
    !(changes.added.is_empty() && changes.deleted.is_empty() && changes.renamed.is_empty())
}

/// Draws again the light links of every current link source the change set
/// does not re-parse (`apply_changes` drew the others), when the file set
/// moved. Precondition: the graph holds a File node for every file of
/// `all_current`. Postcondition on `Ok`: the light links out of every source are
/// the ones a full index of the current tree draws.
pub(super) fn relink_unchanged_sources(
    store: &GraphStore,
    codebase: &Path,
    changes: &ChangeSet,
    all_current: &[Discovered],
) -> Result<(), String> {
    if !file_set_changed(changes) {
        return Ok(());
    }
    let reparsed: HashSet<&str> = changes
        .changed
        .iter()
        .chain(&changes.added)
        .chain(changes.renamed.iter().map(|r| &r.new_file))
        .map(|d| d.rel.as_str())
        .collect();
    let sources: Vec<&Discovered> = all_current
        .iter()
        .filter(|d| {
            !reparsed.contains(d.rel.as_str()) && crate::indexer::light_link::is_link_source(&d.abs)
        })
        .collect();
    if sources.is_empty() {
        return Ok(());
    }
    for d in &sources {
        purge_outbound_light_links(store, &d.rel)?;
    }
    let source_paths: Vec<PathBuf> = sources.iter().map(|d| d.abs.clone()).collect();
    let all_files: Vec<PathBuf> = all_current.iter().map(|d| d.abs.clone()).collect();
    crate::indexer::light_link::link_file_imports_for(store, codebase, &source_paths, &all_files)?;
    Ok(())
}

/// True when the IaC references of a file this pass does not re-parse may
/// differ from a fresh index's: the graph held IaC nodes before the pass, and
/// the pass adds, deletes or renames a file, or re-parses one the IaC pass may
/// read. A graph whose IaC nodes cannot be counted reads as stale (§13: an
/// unknown state is never read as clean). Call it before `apply_changes`.
pub(super) fn iac_references_stale(store: &GraphStore, changes: &ChangeSet) -> bool {
    let iac_file_changed = changes
        .changed
        .iter()
        .any(|d| crate::indexer::iac::is_iac_candidate(&d.abs));
    if !file_set_changed(changes) && !iac_file_changed {
        return false;
    }
    [NODE_IAC_RESOURCE, NODE_IAC_MODULE].iter().any(|label| {
        store
            .execute_query(&format!("MATCH (n:{label}) RETURN n.id LIMIT 1"))
            .map_or(true, |qr| !qr.rows.is_empty())
    })
}

#[cfg(test)]
#[path = "cross_links_tests.rs"]
mod tests;
