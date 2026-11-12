// wiki::processes — process pages built from a resolved + clustered graph
// (issue #196, slice 3).
//
// One `Page` per `Process` node (`cluster_graph` detects them: a BFS along
// call edges from an entry point). Everything is read from the store; nothing
// is recomputed:
//
// * members — the participants of the process, read from every
//   `ParticipatesIn_<Label>_Process` table of `REL_TABLES` (the entry point is
//   one of them, at depth 0), with their `file:line` (criterion 4) taken the
//   same way as on a community page: the `<file>.<ext>::` prefix of the id and
//   its `start_line`.
// * edges — the rows of every table of `REL_TABLES` whose two ends are
//   participant labels and are BOTH participants of the process: the call
//   chain of the process, not every edge its participants have. Provenance is
//   the table's class, as on a community page (criterion 3); a table in
//   neither class (`OBSERVED_CALLS_*`) is not listed. An edge is anchored at
//   the definition of its source, since the store records no line per
//   symbol-level edge.
// * cross-community counts — distinct listed edges whose two ends are members
//   of two different communities, keyed by the community of the target: which
//   communities the process enters, and how often. An end in no community
//   crosses nothing.
//
// A participant the store gives no `file:line` is returned in
// `unanchored_members`, and the process edges it is the source of are counted
// in `unanchored_edges`, one per (process, edge): never omitted in silence.
// The BFS depth of each participant (its step in the chain) is in the store
// but schema v1 has no field for it; members are listed in canonical order.

use std::collections::{BTreeMap, BTreeSet};

use crate::graph_store::{label_declares_column, GraphStore, NODE_PROCESS, REL_TABLES};

use super::communities::{anchor, cell, provenance_of, read_members, relation_of};
use super::skeleton::{Edge, Location, Page, PageKind, Symbol};

/// The process pages of a graph, and what they could not anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPages {
    /// One page per `Process` node, ordered by process id; members and edges
    /// in store order (the skeleton puts them in canonical order).
    pub pages: Vec<Page>,
    /// Ids of the participants left off their pages because the store gives
    /// them no `file:line`, sorted, each once.
    pub unanchored_members: Vec<String>,
    /// Distinct (process, edge) pairs left off their page because the source
    /// of the edge is one of `unanchored_members`.
    pub unanchored_edges: u64,
}

/// A participant symbol: its name, label and anchor when it has one.
struct Participant {
    name: String,
    kind: &'static str,
    location: Option<Location>,
}

/// `(rel table, label)` for every participation table of the schema.
fn participation_tables() -> Vec<(&'static str, &'static str)> {
    REL_TABLES
        .iter()
        .filter(|(rel, _, to)| rel.starts_with("ParticipatesIn_") && *to == NODE_PROCESS)
        .map(|&(rel, from, _)| (rel, from))
        .collect()
}

/// Build one page per process of `store`, which must already be resolved and
/// clustered (`cluster_graph`). A store error is returned, never skipped: a
/// page built from a partial read would be wrong without saying so.
pub fn process_pages(store: &GraphStore) -> Result<ProcessPages, String> {
    let mut pages: BTreeMap<String, Page> = BTreeMap::new();
    let processes = store
        .execute_query("MATCH (p:Process) RETURN p.id, p.name ORDER BY p.id")
        .map_err(|e| format!("wiki: read processes: {e}"))?;
    for row in &processes.rows {
        let (id, name) = (cell(row, 0), cell(row, 1));
        // An empty id identifies no process (tests/community_process_id_audit.rs).
        if id.is_empty() {
            continue;
        }
        pages.insert(id.clone(), Page::new(PageKind::Process, id, name));
    }

    let (participants, steps) = read_participants(store, &pages)?;
    let mut unanchored_members = BTreeSet::new();
    for (process, ids) in &steps {
        let Some(page) = pages.get_mut(process) else {
            continue;
        };
        for id in ids {
            let p = &participants[id];
            match &p.location {
                Some(location) => page.members.push(Symbol {
                    id: id.clone(),
                    name: p.name.clone(),
                    kind: p.kind.to_owned(),
                    location: location.clone(),
                }),
                None => {
                    unanchored_members.insert(id.clone());
                }
            }
        }
    }

    let unanchored_edges = read_edges(store, &participants, &steps, &mut pages)?;
    Ok(ProcessPages {
        pages: pages.into_values().collect(),
        unanchored_members: unanchored_members.into_iter().collect(),
        unanchored_edges,
    })
}

/// Every participant by id, and the participant ids of every process that
/// has a page.
type Participation = (
    BTreeMap<String, Participant>,
    BTreeMap<String, BTreeSet<String>>,
);

/// Read every participation row of a process that has a page.
fn read_participants(
    store: &GraphStore,
    pages: &BTreeMap<String, Page>,
) -> Result<Participation, String> {
    let mut participants = BTreeMap::new();
    let mut steps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (rel, label) in participation_tables() {
        let line = if label_declares_column(label, "start_line") {
            "n.start_line"
        } else {
            "''"
        };
        let rows = store
            .execute_query(&format!(
                "MATCH (n:{label})-[:{rel}]->(p:Process) \
                 RETURN n.id, n.name, {line}, p.id ORDER BY n.id, p.id"
            ))
            .map_err(|e| format!("wiki: read {rel}: {e}"))?;
        for row in &rows.rows {
            let (id, process) = (cell(row, 0), cell(row, 3));
            // An empty node id identifies nothing, and a process with an
            // empty id has no page: such a row is no participation.
            if id.is_empty() || !pages.contains_key(&process) {
                continue;
            }
            let location = anchor(&id, &cell(row, 2));
            participants.entry(id.clone()).or_insert(Participant {
                name: cell(row, 1),
                kind: label,
                location,
            });
            steps.entry(process).or_default().insert(id);
        }
    }
    Ok((participants, steps))
}

/// Put every edge between two participants of the same process on that
/// process's page, count the crossings, and return how many (process, edge)
/// pairs could not be anchored.
fn read_edges(
    store: &GraphStore,
    participants: &BTreeMap<String, Participant>,
    steps: &BTreeMap<String, BTreeSet<String>>,
    pages: &mut BTreeMap<String, Page>,
) -> Result<u64, String> {
    let communities: BTreeMap<String, String> = read_members(store)?
        .into_iter()
        .map(|(id, m)| (id, m.community))
        .collect();
    let mut processes_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (process, ids) in steps {
        for id in ids {
            processes_of.entry(id).or_default().push(process);
        }
    }
    let labels: BTreeSet<&str> = participation_tables().iter().map(|t| t.1).collect();
    let mut unanchored: BTreeSet<(&str, String, String, &str)> = BTreeSet::new();
    let mut crossing: BTreeSet<(&str, String, String, &str)> = BTreeSet::new();
    for &(rel, from, to) in REL_TABLES {
        if !labels.contains(from) || !labels.contains(to) {
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
            let Some(in_processes) = processes_of.get(source.as_str()) else {
                continue;
            };
            for &process in in_processes {
                if !steps[process].contains(&target) {
                    continue;
                }
                let key = (process, source.clone(), target.clone(), relation);
                let ends = (communities.get(&source), communities.get(&target));
                if matches!(ends, (Some(a), Some(b)) if a != b) {
                    crossing.insert(key.clone());
                }
                let Some(location) = participants[&source].location.clone() else {
                    unanchored.insert(key);
                    continue;
                };
                if let Some(page) = pages.get_mut(process) {
                    page.edges.push(Edge::new(
                        (source.clone(), target.clone()),
                        relation,
                        provenance,
                        location,
                    ));
                }
            }
        }
    }
    for (process, _, target, _) in &crossing {
        // A crossing was recorded only with both ends clustered.
        if let (Some(to), Some(page)) = (communities.get(target), pages.get_mut(*process)) {
            *page.cross_community_edges.entry(to.clone()).or_insert(0) += 1;
        }
    }
    Ok(unanchored.len() as u64)
}

#[cfg(test)]
#[path = "processes_tests.rs"]
mod tests;
