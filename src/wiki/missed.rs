// wiki::missed — unresolved call sites attributed to the pages they affect
// (issue #196, criterion 5, the missed-graph half).
//
// A `CallSite` node the resolver left open (`is_resolved = false`) is a
// reference the parser read and the graph does not follow: the call it
// names has no `Calls_*` row, so a page built from the graph shows no edge
// where the code makes one. Every page that lists the symbol whose body
// holds the site (`Defines_Function_CallSite`, `Defines_Method_CallSite`)
// carries a `CoverageGap::Missed { name, location }`: `name` is the callee
// as the source spells it (`callee_name`), `location` the site's own
// `file:line` (the `<file>.<ext>::` prefix of its id, its 1-based `line`).
// The gap is on the page whose edges it truncates, instead of absent from it.
//
// Attribution is by member id, the key the definer edge and the page share,
// so a site lands on a community page and on every process page that lists
// its definer. A site is never dropped: one whose definer is on no page (a
// module-level call, a definer in no community or process) is returned in
// `unattributed`, one the store gives no `file:line` or no callee name in
// `unanchored` (the schema requires a location and a non-empty name on every
// `Missed` gap, criterion 4). A store
// error is returned, never skipped: a page built from a partial read would
// claim fewer gaps than the graph holds without saying so.

use std::collections::{BTreeMap, BTreeSet};

use crate::graph_store::{GraphStore, NODE_CALL_SITE, REL_TABLES};

use super::communities::{anchor, cell};
use super::skeleton::{CoverageGap, Page};

/// What `attribute_missed` could not put on a page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MissedAttribution {
    /// Ids of the open sites whose definer no page lists, sorted.
    pub unattributed: Vec<String>,
    /// Ids of the open sites the store gives no `file:line` or no callee
    /// name, sorted.
    pub unanchored: Vec<String>,
}

/// Add a `Missed` gap to every page of `pages` that lists the definer of an
/// unresolved call site of `store`. A page keeps the gaps it already
/// carries; one gap is added at most once per page.
pub fn attribute_missed(
    pages: &mut [Page],
    store: &GraphStore,
) -> Result<MissedAttribution, String> {
    let sites = open_sites(store)?;
    let definers = site_definers(store, &sites)?;
    let mut attributed: BTreeSet<&str> = BTreeSet::new();
    for page in pages.iter_mut() {
        let members: BTreeSet<&str> = page.members.iter().map(|s| s.id.as_str()).collect();
        for (site, gap) in &sites {
            let Some(gap) = gap else { continue };
            let on_page = definers
                .get(site)
                .is_some_and(|ds| ds.iter().any(|d| members.contains(d.as_str())));
            if !on_page {
                continue;
            }
            attributed.insert(site.as_str());
            if !page.coverage.contains(gap) {
                page.coverage.push(gap.clone());
            }
        }
    }
    let mut out = MissedAttribution::default();
    for (site, gap) in &sites {
        if gap.is_none() {
            out.unanchored.push(site.clone());
        } else if !attributed.contains(site.as_str()) {
            out.unattributed.push(site.clone());
        }
    }
    Ok(out)
}

/// Every unresolved call site, keyed by id, with its gap when the store
/// gives it a `file:line` and a callee name.
fn open_sites(store: &GraphStore) -> Result<BTreeMap<String, Option<CoverageGap>>, String> {
    let rows = store
        .execute_query(&format!(
            "MATCH (cs:{NODE_CALL_SITE}) WHERE cs.is_resolved = false \
             RETURN cs.id, cs.callee_name, cs.line ORDER BY cs.id"
        ))
        .map_err(|e| format!("wiki: read unresolved call sites: {e}"))?;
    let mut sites = BTreeMap::new();
    for row in &rows.rows {
        let (id, name) = (cell(row, 0), cell(row, 1));
        // An empty id identifies no site.
        if id.is_empty() {
            continue;
        }
        let gap = match anchor(&id, &cell(row, 2)) {
            Some(location) if !name.trim().is_empty() => {
                Some(CoverageGap::Missed { name, location })
            }
            _ => None,
        };
        sites.insert(id, gap);
    }
    Ok(sites)
}

/// The definers of every site of `sites`, read from each `Defines_*` table
/// whose target is `CallSite`.
fn site_definers(
    store: &GraphStore,
    sites: &BTreeMap<String, Option<CoverageGap>>,
) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let mut definers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for &(rel, from, to) in REL_TABLES {
        if to != NODE_CALL_SITE || !rel.starts_with("Defines_") {
            continue;
        }
        let rows = store
            .execute_query(&format!(
                "MATCH (d:{from})-[:{rel}]->(cs:{NODE_CALL_SITE}) WHERE cs.is_resolved = false \
                 RETURN cs.id, d.id ORDER BY cs.id, d.id"
            ))
            .map_err(|e| format!("wiki: read {rel}: {e}"))?;
        for row in &rows.rows {
            let (site, definer) = (cell(row, 0), cell(row, 1));
            if definer.is_empty() || !sites.contains_key(&site) {
                continue;
            }
            definers.entry(site).or_default().insert(definer);
        }
    }
    Ok(definers)
}

#[cfg(test)]
#[path = "missed_tests.rs"]
pub(super) mod tests;
