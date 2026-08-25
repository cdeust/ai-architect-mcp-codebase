// search::enrichment — the graph-side data a ranked hit carries.
//
// Split from `search/mod.rs` when that file crossed the §4.1 cap. Both ranking
// paths (hybrid and substring fallback) need the same three things from the
// graph for every hit they return: the row a searchable label yields, the
// community and processes the node belongs to, and the corpus-wide counts the
// score boosts read. They live together because they are one question —
// "what does the graph say about this hit?" — asked once per result.

use crate::graph_store::{
    community_of, label_declares_column, process_names, GraphStore, SymbolMatch,
};
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

/// The weights one ranking path applies to the two membership boosts.
///
/// The rule is shared; the weights are not, because the two scorers produce
/// scores on different scales — RRF fuses reciprocal ranks, the substring
/// scorer returns a 0..1 overlap score — so the same nudge would swamp one and
/// vanish in the other.
pub(super) struct BoostWeights {
    /// Added when the hit sits in a community smaller than `SMALL_COMMUNITY`.
    pub(super) small_community: f64,
    /// Multiplied by the process count, saturating at `MAX_BOOSTED_PROCESSES`.
    pub(super) per_process: f64,
}

/// Member count below which a community counts as small — a hit inside a tight
/// cluster is a likelier answer than one inside a catch-all. A community whose
/// size is unknown is treated as large.
const SMALL_COMMUNITY: u64 = 20;

/// Community size assumed when the corpus scan has no entry for the id.
const ASSUMED_COMMUNITY_SIZE: u64 = 100;

/// Process participations past which the boost stops growing.
const MAX_BOOSTED_PROCESSES: usize = 3;

impl RankBoosts {
    /// The community + process nudge for one hit, under `weights`.
    ///
    /// Shared by both ranking paths: the arithmetic was copy-pasted between
    /// them with only the two constants differing, which is how one path comes
    /// to be tuned and the other silently left behind.
    pub(super) fn boost_for(
        &self,
        qualified_name: &str,
        community_id: Option<&str>,
        weights: &BoostWeights,
    ) -> f64 {
        let community = match community_id {
            Some(cid) => {
                let size = self
                    .community_sizes
                    .get(cid)
                    .copied()
                    .unwrap_or(ASSUMED_COMMUNITY_SIZE);
                if size < SMALL_COMMUNITY {
                    weights.small_community
                } else {
                    0.0
                }
            }
            None => 0.0,
        };
        let participations = self
            .process_counts
            .get(qualified_name)
            .copied()
            .unwrap_or(0)
            .min(MAX_BOOSTED_PROCESSES);
        community + weights.per_process * (participations as f64)
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
/// range at all.
///
/// The has-lines answer is derived from the node table's declared columns, not
/// from a hand-written list of labels: binding `n.start_line` on a label whose
/// table lacks it is a hard Binder exception, and a hand-written partition
/// drifts from the schema (it had Constant and TypeAlias on the wrong side,
/// dropping the line numbers those tables do declare).
pub(super) fn return_clause(label: &str) -> (&'static str, bool) {
    let has_lines = label_declares_column(label, "start_line");
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

/// The community this node belongs to, or `None`.
///
/// `community_of` caps the probe at one row, which matters here: this runs once
/// per candidate on the substring fallback, and that path scans every node of
/// every searchable label.
pub(super) fn lookup_community(store: &GraphStore, label: &str, node_id: &str) -> Option<String> {
    community_of(store, label, SymbolMatch::Id(node_id)).map(|c| c.id)
}

/// The processes this node participates in. Empty for every label but
/// `Function` and `Method`, which `process_names` enforces.
pub(super) fn lookup_processes(store: &GraphStore, label: &str, node_id: &str) -> Vec<String> {
    process_names(store, label, SymbolMatch::Id(node_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard for the wrong hand-written partition. `return_clause`
    /// used to decide "has a line range" from the literal list
    /// [Function, Method, Struct, Enum, Trait], which put Constant and
    /// TypeAlias on the wrong side even though `COLS_CONSTANT` and
    /// `COLS_TYPE_ALIAS` both declare `start_line`/`end_line`. Every search
    /// result and get_context answer for those two kinds therefore reported no
    /// line numbers — measured on this repository's own src/ (2026-08-25),
    /// 393 Constant nodes carry a start_line the old clause never asked for.
    ///
    /// This test fails on the pre-fix code.
    #[test]
    fn constants_and_type_aliases_report_their_line_range() {
        for label in ["Constant", "TypeAlias"] {
            let (clause, has_lines) = return_clause(label);
            assert!(has_lines, "{label} declares start_line in its node table");
            assert!(
                clause.contains("n.start_line") && clause.contains("n.end_line"),
                "{label} must ask for the line columns it declares: {clause}"
            );
        }
    }

    /// The other side of the same partition: Module genuinely declares no line
    /// columns, and binding them would be a hard Binder exception rather than a
    /// NULL, so the clause must keep leaving them out.
    #[test]
    fn module_still_omits_the_line_columns_it_does_not_declare() {
        let (clause, has_lines) = return_clause("Module");
        assert!(!has_lines);
        assert!(!clause.contains("start_line"), "clause: {clause}");
    }
}
