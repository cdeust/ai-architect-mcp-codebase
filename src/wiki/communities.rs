// wiki::communities — community pages built from a resolved + clustered
// graph (issue #196, slice 2).
//
// One `Page` per `Community` node. Everything is read from the store; nothing
// is recomputed:
//
// * members — the symbols of every `MemberOf_<Label>_Community` table of
//   `REL_TABLES`, with their `file:line` (criterion 4): the file is the
//   `<file>.<ext>::` prefix of the node id, the line its `start_line`.
// * edges — the rows of every table of `REL_TABLES` whose two ends are member
//   labels, listed on the page of their source. Provenance is the table's
//   class: a structural table (Defines, HasMethod, ...) is `extracted`, a
//   resolution table (Calls, Uses, Imports, Implements, Extends) `inferred`
//   (criterion 3). A table in neither class (`OBSERVED_CALLS_*`, runtime
//   traces) has no provenance either value names, so it is not listed.
// * cross-community counts — distinct (source, target, relation) edges from
//   a member to a member of another community, keyed by that community's id.
//
// The store records no line per symbol-level edge, so an edge is anchored at
// the definition of its source symbol: the claim "a calls b" is made by the
// body that starts there. A member the store gives no `file:line` (a `Module`
// has no `start_line`; an id with no known file extension has no file) cannot
// be listed without breaking criterion 4, and leaving it out silently would
// hide it: it is returned in `unanchored_members`, and the edges it is the
// source of are counted in `unanchored_edges` (they still count in the
// cross-community totals, which need no line).

use std::collections::{BTreeMap, BTreeSet};

use crate::graph_store::{
    is_resolution_rel, is_structural_provenance_rel, label_declares_column, GraphStore,
    NODE_COMMUNITY, REL_TABLES,
};
use crate::language_provider::extract_file_prefix;

use super::skeleton::{Edge, Location, Page, PageKind, Provenance, Symbol};

/// The community pages of a graph, and what they could not anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityPages {
    /// One page per `Community` node, members and edges in store order (the
    /// skeleton puts them in canonical order).
    pub pages: Vec<Page>,
    /// Ids of the members left off their page because the store gives them
    /// no `file:line`, sorted.
    pub unanchored_members: Vec<String>,
    /// Distinct edges left off their page because their source is one of
    /// `unanchored_members`.
    pub unanchored_edges: u64,
}

/// A clustered symbol: its page, and its anchor when it has one.
struct Member {
    community: String,
    location: Option<Location>,
}

/// The provenance of an edge table, or `None` when it is in neither class.
pub(super) fn provenance_of(rel: &str) -> Option<Provenance> {
    if is_structural_provenance_rel(rel) {
        Some(Provenance::Extracted)
    } else if is_resolution_rel(rel) {
        Some(Provenance::Inferred)
    } else {
        None
    }
}

/// The relation name an edge table carries: `Calls_Function_Method` -> `Calls`.
fn relation_of(rel: &str) -> &str {
    rel.split('_').next().unwrap_or(rel)
}

/// `(rel table, label)` for every membership table of the schema.
fn membership_tables() -> Vec<(&'static str, &'static str)> {
    REL_TABLES
        .iter()
        .filter(|(rel, _, to)| rel.starts_with("MemberOf_") && *to == NODE_COMMUNITY)
        .map(|&(rel, from, _)| (rel, from))
        .collect()
}

/// The `file:line` of a node, when the store has both halves.
fn anchor(id: &str, start_line: &str) -> Option<Location> {
    let file = extract_file_prefix(id)?;
    let line = start_line.trim().parse::<u32>().ok()?;
    Location::new(file, line).ok()
}

/// Build one page per community of `store`, which must already be resolved
/// and clustered (`cluster_graph`). A store error is returned, never skipped:
/// a page built from a partial read would be wrong without saying so.
pub fn community_pages(store: &GraphStore) -> Result<CommunityPages, String> {
    let mut pages: BTreeMap<String, Page> = BTreeMap::new();
    let communities = store
        .execute_query("MATCH (c:Community) RETURN c.id, c.name ORDER BY c.id")
        .map_err(|e| format!("wiki: read communities: {e}"))?;
    for row in &communities.rows {
        let (id, name) = (cell(row, 0), cell(row, 1));
        // An empty id identifies no community (tests/community_process_id_audit.rs).
        if id.is_empty() {
            continue;
        }
        pages.insert(id.clone(), Page::new(PageKind::Community, id, name));
    }

    let members = read_members(store, &mut pages)?;
    let mut unanchored_members: Vec<String> = members
        .iter()
        .filter(|(_, m)| m.location.is_none())
        .map(|(id, _)| id.clone())
        .collect();
    unanchored_members.sort_unstable();

    let tally = read_edges(store, &members, &mut pages)?;
    for (source, target, _) in &tally.crossing {
        let to = &members[target].community;
        if let Some(page) = pages.get_mut(&members[source].community) {
            *page.cross_community_edges.entry(to.clone()).or_insert(0) += 1;
        }
    }

    Ok(CommunityPages {
        pages: pages.into_values().collect(),
        unanchored_members,
        unanchored_edges: tally.unanchored.len() as u64,
    })
}

/// `(source, target, relation)` of an edge.
type EdgeKey = (String, String, String);

/// What `read_edges` saw besides the edges it put on pages.
struct EdgeTally {
    /// Edges whose source has no `file:line`.
    unanchored: BTreeSet<EdgeKey>,
    /// Edges between members of two different communities.
    crossing: BTreeSet<EdgeKey>,
}

/// Put every symbol-to-symbol edge with a provenance on the page of its
/// source, and tally the edges it could not anchor and those that cross.
fn read_edges(
    store: &GraphStore,
    members: &BTreeMap<String, Member>,
    pages: &mut BTreeMap<String, Page>,
) -> Result<EdgeTally, String> {
    let mut tally = EdgeTally {
        unanchored: BTreeSet::new(),
        crossing: BTreeSet::new(),
    };
    let member_labels: BTreeSet<&str> = membership_tables().iter().map(|t| t.1).collect();
    for &(rel, from, to) in REL_TABLES {
        if !member_labels.contains(from) || !member_labels.contains(to) {
            continue;
        }
        let Some(provenance) = provenance_of(rel) else {
            continue;
        };
        let relation = relation_of(rel);
        let rows = store
            .execute_query(&format!(
                "MATCH (a:{from})-[:{rel}]->(b:{to}) RETURN a.id, b.id ORDER BY a.id, b.id"
            ))
            .map_err(|e| format!("wiki: read {rel}: {e}"))?;
        for row in &rows.rows {
            let (source, target) = (cell(row, 0), cell(row, 1));
            // A source in no community has no page to be listed on.
            let Some(member) = members.get(&source) else {
                continue;
            };
            let key = (source, target, relation.to_owned());
            if members
                .get(&key.1)
                .is_some_and(|t| t.community != member.community)
            {
                tally.crossing.insert(key.clone());
            }
            let Some(location) = member.location.clone() else {
                tally.unanchored.insert(key);
                continue;
            };
            if let Some(page) = pages.get_mut(&member.community) {
                page.edges
                    .push(Edge::new((key.0, key.1), relation, provenance, location));
            }
        }
    }
    Ok(tally)
}

/// Read every membership row, add the anchored members to their page and
/// return the community and anchor of every member, keyed by node id.
fn read_members(
    store: &GraphStore,
    pages: &mut BTreeMap<String, Page>,
) -> Result<BTreeMap<String, Member>, String> {
    let mut members = BTreeMap::new();
    for (rel, label) in membership_tables() {
        // lbug refuses a property the label's table does not declare, so a
        // label with no line range (Module) reads an empty one instead.
        let line = if label_declares_column(label, "start_line") {
            "n.start_line"
        } else {
            "''"
        };
        let rows = store
            .execute_query(&format!(
                "MATCH (n:{label})-[:{rel}]->(c:Community) \
                 RETURN n.id, n.name, {line}, c.id ORDER BY n.id"
            ))
            .map_err(|e| format!("wiki: read {rel}: {e}"))?;
        for row in &rows.rows {
            let (id, name, community) = (cell(row, 0), cell(row, 1), cell(row, 3));
            // An empty node or community id identifies nothing: such a row is
            // no membership, so its symbol is in no community.
            if id.is_empty() || community.is_empty() {
                continue;
            }
            let location = anchor(&id, &cell(row, 2));
            if let (Some(location), Some(page)) = (&location, pages.get_mut(&community)) {
                page.members.push(Symbol {
                    id: id.clone(),
                    name,
                    kind: label.to_owned(),
                    location: location.clone(),
                });
            }
            members.insert(
                id,
                Member {
                    community,
                    location,
                },
            );
        }
    }
    Ok(members)
}

fn cell(row: &[String], i: usize) -> String {
    row.get(i).cloned().unwrap_or_default()
}

#[cfg(test)]
#[path = "communities_tests.rs"]
mod tests;
