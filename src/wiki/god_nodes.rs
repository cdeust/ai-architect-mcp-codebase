// wiki::god_nodes — god-node pages built from a resolved + clustered graph
// (issue #196, slice 6).
//
// A god node is a clustered symbol whose degree is a far outlier of the
// degree distribution of all clustered symbols: degree > Q3 + 3·IQR, Tukey's
// "far out" fence (J. W. Tukey, *Exploratory Data Analysis*, 1977, §2). The
// decision and its alternatives are recorded on issue #196
// (`<!-- ci-autopilot decision -->`). The fence is relative to the graph, so
// no absolute constant is chosen here; the factor 3 is Tukey's.
//
// * degree — the number of distinct (source, target, relation) edges with a
//   provenance (the edges a community page lists: structural tables are
//   `extracted`, resolution tables `inferred`, `OBSERVED_CALLS_*` neither)
//   whose two ends are clustered symbols, in or out. A self-edge counts once.
// * quartiles — Hyndman & Fan type 7 (linear interpolation between order
//   statistics; R's and NumPy's default), over every clustered symbol,
//   degree 0 included. With h = (n − 1)·p the fractional part of h is a
//   multiple of 1/4, so every quartile and the fence are computed exactly in
//   quarter units: no floating point, so no rounding can move a symbol across
//   the fence on another platform.
// * the comparison is strict: a degree equal to the fence is not an outlier.
//   When IQR = 0 the fence is Q3 and only degrees above it qualify (a star:
//   the hub, never its leaves); a graph where every symbol has the same
//   degree has none.
//
// One `Page` per god node: id the symbol id, title its name, the symbol as
// its only member, every incident edge (anchored at the definition of its
// source, as on the other pages), and the cross-community counts of those
// edges keyed by the community of the other end. A god node the store gives
// no `file:line` has no page (criterion 4) and is returned in
// `unanchored_members`; an incident edge whose source has no `file:line` is
// counted in `unanchored_edges`. Coverage gaps are attributed afterwards by
// `attribute_parse_incomplete` and `attribute_missed`, which match pages by
// member, whatever their kind.

use std::collections::{BTreeMap, BTreeSet};

use crate::graph_store::{GraphStore, REL_TABLES};

use super::communities::{cell, membership_tables, provenance_of, read_members, relation_of};
use super::skeleton::{Edge, Page, PageKind, Provenance, Symbol};

/// The god-node pages of a graph, the fence that selected them, and what
/// they could not anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GodNodePages {
    /// One page per god node, ordered by symbol id.
    pub pages: Vec<Page>,
    /// The fence over the degrees of the clustered symbols; `None` when the
    /// graph has no clustered symbol.
    pub fence: Option<Fence>,
    /// Ids of the god nodes left without a page because the store gives them
    /// no `file:line`, sorted.
    pub unanchored_members: Vec<String>,
    /// Distinct edges incident to a god node with a page, left off it because
    /// their source has no `file:line`.
    pub unanchored_edges: u64,
}

/// Tukey's far-out fence, Q3 + 3·(Q3 − Q1), with both quartiles held in
/// quarter units (4·Q is an integer for type-7 quartiles of integers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fence {
    q1_quarters: u64,
    q3_quarters: u64,
}

impl Fence {
    /// The type-7 quartiles of `degrees` (any order); `None` when empty.
    pub fn of(degrees: &[u64]) -> Option<Self> {
        let mut sorted = degrees.to_vec();
        sorted.sort_unstable();
        Some(Self {
            q1_quarters: quartile_quarters(&sorted, 1)?,
            q3_quarters: quartile_quarters(&sorted, 3)?,
        })
    }

    /// 4·Q1.
    pub fn q1_quarters(&self) -> u64 {
        self.q1_quarters
    }

    /// 4·Q3.
    pub fn q3_quarters(&self) -> u64 {
        self.q3_quarters
    }

    /// 4·(Q3 + 3·IQR).
    pub fn limit_quarters(&self) -> u64 {
        self.q3_quarters + 3 * (self.q3_quarters - self.q1_quarters)
    }

    /// Whether `degree` lies strictly beyond the fence.
    pub fn exceeds(&self, degree: u64) -> bool {
        4 * degree > self.limit_quarters()
    }
}

/// 4 × the type-7 `k`-th quartile (k ∈ {1, 3}) of `sorted`.
/// h = (n − 1)·k/4; Q = x[⌊h⌋] + (h − ⌊h⌋)·(x[⌊h⌋+1] − x[⌊h⌋]) (0-based).
fn quartile_quarters(sorted: &[u64], k: u64) -> Option<u64> {
    let n = sorted.len() as u64;
    let last = n.checked_sub(1)?;
    let h4 = last * k; // 4·h
    let (lo, frac) = ((h4 / 4) as usize, h4 % 4);
    let x_lo = sorted[lo];
    // frac > 0 implies h < n − 1, so lo + 1 is in range.
    let x_hi = if frac == 0 { x_lo } else { sorted[lo + 1] };
    Some(4 * x_lo + frac * (x_hi - x_lo))
}

/// `(source, target, relation)` of an edge, with its provenance.
type Incident = BTreeMap<(String, String, String), Provenance>;

/// Build one page per god node of `store`, which must already be resolved
/// and clustered (`cluster_graph`). A store error is returned, never skipped.
pub fn god_node_pages(store: &GraphStore) -> Result<GodNodePages, String> {
    let members = read_members(store)?;
    let edges = read_edges(store, &members)?;

    let mut degree: BTreeMap<&str, u64> = members.keys().map(|id| (id.as_str(), 0)).collect();
    for (source, target, _) in edges.keys() {
        *degree.entry(source.as_str()).or_default() += 1;
        if target != source {
            *degree.entry(target.as_str()).or_default() += 1;
        }
    }
    let degrees: Vec<u64> = degree.values().copied().collect();
    let Some(fence) = Fence::of(&degrees) else {
        return Ok(GodNodePages {
            pages: Vec::new(),
            fence: None,
            unanchored_members: Vec::new(),
            unanchored_edges: 0,
        });
    };

    let mut pages: BTreeMap<&str, Page> = BTreeMap::new();
    let mut unanchored_members = Vec::new();
    for (&id, &d) in &degree {
        if !fence.exceeds(d) {
            continue;
        }
        let member = &members[id];
        let Some(location) = member.location.clone() else {
            unanchored_members.push(id.to_owned());
            continue;
        };
        let mut page = Page::new(PageKind::GodNode, id, member.name.clone());
        page.members.push(Symbol {
            id: id.to_owned(),
            name: member.name.clone(),
            kind: member.kind.to_owned(),
            location,
        });
        pages.insert(id, page);
    }

    let mut unanchored_edges: BTreeSet<&(String, String, String)> = BTreeSet::new();
    for (key, &provenance) in &edges {
        let (source, target, relation) = key;
        let ends = if source == target {
            vec![source.as_str()]
        } else {
            vec![source.as_str(), target.as_str()]
        };
        for end in ends {
            let Some(page) = pages.get_mut(end) else {
                continue;
            };
            let other = if end == source { target } else { source };
            let (own, theirs) = (&members[end].community, &members[other].community);
            if own != theirs {
                *page
                    .cross_community_edges
                    .entry(theirs.clone())
                    .or_insert(0) += 1;
            }
            match members[source].location.clone() {
                Some(location) => page.edges.push(Edge::new(
                    (source.clone(), target.clone()),
                    relation.clone(),
                    provenance,
                    location,
                )),
                None => {
                    unanchored_edges.insert(key);
                }
            }
        }
    }

    Ok(GodNodePages {
        pages: pages.into_values().collect(),
        fence: Some(fence),
        unanchored_members,
        unanchored_edges: unanchored_edges.len() as u64,
    })
}

/// Every distinct edge with a provenance whose two ends are clustered.
fn read_edges(
    store: &GraphStore,
    members: &BTreeMap<String, super::communities::Member>,
) -> Result<Incident, String> {
    let labels: BTreeSet<&str> = membership_tables().iter().map(|t| t.1).collect();
    let mut edges = Incident::new();
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
            if !members.contains_key(&source) || !members.contains_key(&target) {
                continue;
            }
            edges
                .entry((source, target, relation.to_owned()))
                .or_insert(provenance);
        }
    }
    Ok(edges)
}

#[cfg(test)]
#[path = "god_nodes_tests.rs"]
mod tests;
