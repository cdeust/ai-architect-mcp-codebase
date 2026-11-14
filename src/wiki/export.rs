// wiki::export — the whole wiki skeleton of a resolved + clustered graph
// (issue #196, slice 7).
//
// The earlier slices each build one part: the community pages
// (`communities`), the process pages (`processes`), the god-node pages
// (`god_nodes`), and the two coverage attributions (`coverage` for the
// `parse_incomplete` files of the index's coverage record, `missed` for the
// call sites the resolver left open). `export` runs all of them on one store,
// in that order, attributes the gaps to every page whatever its kind, and
// builds the validated, canonical `WikiSkeleton`: the artifact a caller
// serialises with `to_json`.
//
// Nothing is dropped in silence. What a builder could not anchor and what an
// attribution could not place is returned beside the skeleton, in
// `ExportReport`, with the god-node fence that selected the god-node pages,
// so a caller can say that a gap exists and why it is on no page. Schema v1
// has no field for those lists; they are the caller's to show.
//
// The function reads the store and the coverage record it is given, and
// nothing else: no file, no network, no model client (criteria 1 and 2). It
// is deterministic: the same store and record give a byte-identical
// `to_json` (criterion 6), since every builder reads in store order and the
// skeleton canonicalises every list. The MCP tool that exposes the export is
// `export_wiki_skeleton` (`wiki_handlers`), slice 8 of #196.

use crate::graph_store::GraphStore;
use crate::indexer::coverage::CoverageReport;

use super::communities::community_pages;
use super::coverage::attribute_parse_incomplete;
use super::god_nodes::{god_node_pages, Fence};
use super::missed::attribute_missed;
use super::processes::process_pages;
use super::skeleton::WikiSkeleton;

/// What the skeleton could not hold, per builder and per attribution.
/// Every list is sorted, as its builder returns it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    /// Community members with no `file:line`, left off their page.
    pub community_unanchored_members: Vec<String>,
    /// Community edges left off their page: their source is unanchored.
    pub community_unanchored_edges: u64,
    /// Process participants with no `file:line`, left off their pages.
    pub process_unanchored_members: Vec<String>,
    /// (process, edge) pairs left off their page: their source is unanchored.
    pub process_unanchored_edges: u64,
    /// God nodes with no `file:line`, so with no page.
    pub god_node_unanchored_members: Vec<String>,
    /// Edges incident to a god node with a page, left off it: their source
    /// is unanchored.
    pub god_node_unanchored_edges: u64,
    /// The fence that selected the god nodes; `None` when the graph has no
    /// clustered symbol.
    pub god_node_fence: Option<Fence>,
    /// `parse_incomplete` files no page lists a member in.
    pub parse_incomplete_unattributed: Vec<String>,
    /// Open call sites whose definer no page lists.
    pub missed_unattributed: Vec<String>,
    /// Open call sites the store gives no `file:line` or no callee name.
    pub missed_unanchored: Vec<String>,
}

/// The skeleton of `store`, and what it could not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub skeleton: WikiSkeleton,
    pub report: ExportReport,
}

/// Build the wiki skeleton of `store`, which must already be resolved and
/// clustered (`cluster_graph`), with the gaps of `coverage`, the coverage
/// record of the same index. A store error or an invalid page is returned,
/// never skipped: a skeleton built from a partial read would be wrong without
/// saying so.
pub fn export(store: &GraphStore, coverage: &CoverageReport) -> Result<Export, String> {
    let communities = community_pages(store)?;
    let processes = process_pages(store)?;
    let god_nodes = god_node_pages(store)?;

    let mut pages = communities.pages;
    pages.extend(processes.pages);
    pages.extend(god_nodes.pages);
    let partial = attribute_parse_incomplete(&mut pages, coverage);
    let missed = attribute_missed(&mut pages, store)?;

    let skeleton = WikiSkeleton::new(pages).map_err(|e| format!("wiki: skeleton: {e}"))?;
    Ok(Export {
        skeleton,
        report: ExportReport {
            community_unanchored_members: communities.unanchored_members,
            community_unanchored_edges: communities.unanchored_edges,
            process_unanchored_members: processes.unanchored_members,
            process_unanchored_edges: processes.unanchored_edges,
            god_node_unanchored_members: god_nodes.unanchored_members,
            god_node_unanchored_edges: god_nodes.unanchored_edges,
            god_node_fence: god_nodes.fence,
            parse_incomplete_unattributed: partial.unattributed,
            missed_unattributed: missed.unattributed,
            missed_unanchored: missed.unanchored,
        },
    })
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
