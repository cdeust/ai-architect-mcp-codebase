// search::enrichment — the graph-side data a ranked hit carries.
//
// Split from `search/mod.rs` when that file crossed the §4.1 cap. Both ranking
// paths (hybrid and substring fallback) need the same three things from the
// graph for every hit they return: the row a searchable label yields, the
// community and processes the node belongs to, and the corpus-wide counts the
// score boosts read. They live together because they are one question —
// "what does the graph say about this hit?" — asked once per result.

use crate::graph_store::{community_ids, process_names, GraphStore, SymbolMatch};
use std::collections::HashMap;

/// Corpus-wide inputs to the score boosts, loaded ONCE per search rather than
/// once per hit: both are full-graph scans.
pub(super) struct RankBoosts {
    pub(super) community_sizes: HashMap<String, u64>,
    pub(super) process_counts: HashMap<String, usize>,
}

impl RankBoosts {
    pub(super) fn load(store: &GraphStore) -> Self {
        RankBoosts {
            community_sizes: load_community_sizes(store),
            process_counts: load_process_counts(store),
        }
    }
}

// ---------------------------------------------------------------------------
// Row shape — which columns a searchable label yields, and how to read them
// ---------------------------------------------------------------------------

/// One searchable node's row, decoded. `start_line`/`end_line` are `None` for
/// the labels that carry no line range.
pub(super) struct SymbolRow {
    pub(super) qualified_name: String,
    pub(super) name: String,
    pub(super) id: String,
    pub(super) start_line: Option<u64>,
    pub(super) end_line: Option<u64>,
}

/// The `RETURN` clause for `label`, and whether that label carries a line
/// range at all. `Module`/`Constant`/`TypeAlias` do not, and binding
/// `n.start_line` on them is a hard Binder exception rather than a NULL — so
/// the two answers must be decided together, which is why one function gives
/// both.
pub(super) fn return_clause(label: &str) -> (&'static str, bool) {
    let has_lines = matches!(label, "Function" | "Method" | "Struct" | "Enum" | "Trait");
    let clause = if has_lines {
        "n.qualified_name, n.name, n.id, n.start_line, n.end_line"
    } else {
        "n.qualified_name, n.name, n.id"
    };
    (clause, has_lines)
}

/// Decodes one row produced by [`return_clause`]. `None` when the row is too
/// short to carry the three mandatory columns.
pub(super) fn decode_row(row: &[String], has_lines: bool) -> Option<SymbolRow> {
    if row.len() < 3 {
        return None;
    }
    let (start_line, end_line) = if has_lines && row.len() >= 5 {
        (parse_opt_u64(&row[3]), parse_opt_u64(&row[4]))
    } else {
        (None, None)
    };
    Some(SymbolRow {
        qualified_name: row[0].clone(),
        name: row[1].clone(),
        id: row[2].clone(),
        start_line,
        end_line,
    })
}

pub(super) fn parse_opt_u64(s: &str) -> Option<u64> {
    s.parse::<u64>().ok()
}

pub(super) fn load_community_sizes(store: &GraphStore) -> HashMap<String, u64> {
    let mut sizes = HashMap::new();
    let cypher = "MATCH (c:Community) RETURN c.id, c.member_count";
    if let Ok(qr) = store.execute_query(cypher) {
        for row in &qr.rows {
            if row.len() >= 2 {
                if let Ok(count) = row[1].parse::<u64>() {
                    sizes.insert(row[0].clone(), count);
                }
            }
        }
    }
    sizes
}

pub(super) fn load_process_counts(store: &GraphStore) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let labels = ["Function", "Method"];
    for label in labels {
        let cypher = format!(
            "MATCH (n:{label})-[:ParticipatesIn_{label}_Process]->(p:Process) \
             RETURN n.qualified_name, count(p)"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if row.len() >= 2 {
                    if let Ok(c) = row[1].parse::<usize>() {
                        counts.insert(row[0].clone(), c);
                    }
                }
            }
        }
    }
    counts
}

/// The community this node belongs to, or `None`. Clustering assigns one per
/// node, so taking the first of `community_ids` is the whole answer.
pub(super) fn lookup_community(store: &GraphStore, label: &str, node_id: &str) -> Option<String> {
    community_ids(store, label, SymbolMatch::Id(node_id))
        .into_iter()
        .next()
}

/// The processes this node participates in. Empty for every label but
/// `Function` and `Method`, which `process_names` enforces.
pub(super) fn lookup_processes(store: &GraphStore, label: &str, node_id: &str) -> Vec<String> {
    process_names(store, label, SymbolMatch::Id(node_id))
}
