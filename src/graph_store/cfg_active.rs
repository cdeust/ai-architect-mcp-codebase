// graph_store::cfg_active: whether the default build compiles a twin item, and
// how the read tools see the twins of one item (issue #353, part B).
//
// A twin is a node whose id carries a `#cfg(<gate>)` suffix (see `cfg_twins`).
// The indexer decides, once per index pass, whether the package's default
// features compile it and writes `active`, `inactive` or `unknown` into the
// `cfg_active` column; the resolver reads it to pick the compiled twin of a call
// (`resolver::cfg_select`), and `get_impact`, `get_symbol` and `index_status`
// show it. `''` means "not decided": a node that is not a twin, and any graph
// written before the column existed. Every reader treats a missing column and
// `''` as `unknown`, never as `active`.
//
// The value belongs to the DEFAULT build profile only (default features, the
// features `cargo metadata` reports). It says nothing about `cfg(kani)`, `test`
// or a target; those stay `unknown` because the source alone does not decide
// them. `indexer::cfg_active` documents where a second profile would plug in.

use std::collections::{BTreeMap, HashMap};

use super::cfg_twins::{closing_paren, strip_cfg_gates, TWIN_MARK};
use super::{cypher_str, GraphStore, BULK_BATCH_SIZE, CFG_GATE_LABELS};
use lbug::{LogicalType, Value};

/// `resolution_method` of an edge chosen among `#[cfg]` twins by the build
/// profile or the caller's gate: the label of
/// `ambiguity_policy::Evidence::CfgSelected` (a test there pins the text).
pub const RESOLUTION_CFG_SELECTED: &str = "cfg-selected";

/// The default profile compiles the item.
pub const CFG_ACTIVE: &str = "active";
/// The default profile compiles the item out.
pub const CFG_INACTIVE: &str = "inactive";
/// The source and the default profile do not decide it.
pub const CFG_UNKNOWN: &str = "unknown";

const ACTIVE_COLUMN_TYPE: &str = "STRING DEFAULT ''";

/// The compact gates written in `qn`, one per `#cfg(..)` suffix, in path order:
/// `src/lib.rs::m#cfg(unix)::f#cfg(kani)` gives `["unix", "kani"]`. Their
/// conjunction is the condition under which the item exists.
pub(crate) fn cfg_gates_in(qn: &str) -> Vec<&str> {
    let mut gates = Vec::new();
    let mut rest = qn;
    while let Some(start) = rest.find(TWIN_MARK) {
        let after = &rest[start + TWIN_MARK.len()..];
        let Some(close) = closing_paren(after) else {
            break;
        };
        gates.push(&after[..close]);
        rest = &after[close + 1..];
    }
    gates
}

/// One twin node as the read tools show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwinRow {
    pub id: String,
    pub label: String,
    /// The compact gate under which the item exists: its own `cfg_gate`, or,
    /// for a member of a twin container, the conjunction of the gates in its id.
    pub cfg_gate: String,
    /// `active`, `inactive` or `unknown`; never empty.
    pub cfg_active: String,
}

impl TwinRow {
    /// The shape every read tool shows a twin in.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "cfg_gate": self.cfg_gate,
            "cfg_active": self.cfg_active,
        })
    }
}

/// What `index_status` reports about the twins of a graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CfgTwinSummary {
    /// Names that more than one node holds once the suffixes are stripped.
    pub sets: u64,
    pub members: u64,
    pub active: u64,
    pub inactive: u64,
    pub unknown: u64,
    /// Open `CallSite`s left so on purpose: every candidate is a twin and the
    /// default profile does not pick one.
    pub unresolved_sites: u64,
    /// Twin nodes the default profile compiles out, capped, in id order.
    pub inactive_examples: Vec<TwinRow>,
}

/// How many inactive twins `cfg_twin_summary` lists; the count is exact.
pub const CFG_TWIN_EXAMPLE_CAP: usize = 25;

fn normalize(active: &str) -> String {
    match active {
        CFG_ACTIVE | CFG_INACTIVE => active.to_string(),
        _ => CFG_UNKNOWN.to_string(),
    }
}

fn display_gate(own: &str, id: &str) -> String {
    if !own.is_empty() {
        return own.to_string();
    }
    match cfg_gates_in(id).as_slice() {
        [] => String::new(),
        [one] => (*one).to_string(),
        many => format!("all({})", many.join(",")),
    }
}

impl GraphStore {
    /// `(label, id)` of every node whose id carries a twin suffix. A label the
    /// graph does not have is skipped.
    pub fn cfg_twin_ids(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for label in CFG_GATE_LABELS {
            let cypher = format!(
                "MATCH (n:{label}) WHERE n.id CONTAINS {} RETURN n.id",
                cypher_str(TWIN_MARK)
            );
            if let Ok(rows) = self.execute_query(&cypher) {
                out.extend(
                    rows.rows
                        .into_iter()
                        .map(|r| (label.to_string(), r[0].clone())),
                );
            }
        }
        out
    }

    /// Writes `cfg_active` for every `(label, id, value)`. The column is added
    /// first on a graph written before it existed. Values are this module's
    /// three constants, never caller input.
    pub fn write_cfg_active(
        &self,
        assignments: &[(String, String, &'static str)],
    ) -> Result<(), String> {
        let mut grouped: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
        for (label, id, value) in assignments {
            grouped.entry((label, value)).or_default().push(id);
        }
        for ((label, value), ids) in grouped {
            self.ensure_node_column(label, "cfg_active", ACTIVE_COLUMN_TYPE)?;
            let cypher = format!(
                "UNWIND $rows AS rid MATCH (n:{label} {{id: rid}}) SET n.cfg_active = {}",
                cypher_str(value)
            );
            for chunk in ids.chunks(BULK_BATCH_SIZE) {
                let values = chunk
                    .iter()
                    .map(|id| Value::String((*id).to_string()))
                    .collect();
                self.run_prepared(&cypher, Value::List(LogicalType::String, values))?;
            }
        }
        Ok(())
    }

    /// Undoes what an earlier resolve chose among twins: every `cfg-selected`
    /// call row goes, and the per-site rows' call sites are opened again, so the
    /// pass that follows decides them under the current build profile. The
    /// choice depends on `cfg_active` and on the gates in the ids, and an edit
    /// of `Cargo.toml` changes the first without changing any file, so a row an
    /// earlier run wrote can name a twin the build no longer compiles.
    /// Same precedent as `reset_macro_expansion`.
    pub(crate) fn reset_cfg_selected(&self) -> Result<(), String> {
        for &(rel, from, _) in super::schema::REL_TABLES {
            if !rel.starts_with("Calls_") {
                continue;
            }
            if from == super::schema::NODE_CALL_SITE {
                self.ensure_node_column("CallSite", "is_resolved", "BOOLEAN DEFAULT false")?;
                self.run(&format!(
                    "MATCH (cs:CallSite)-[r:{rel}]->() WHERE r.resolution_method = {} \
                     SET cs.is_resolved = false",
                    cypher_str(RESOLUTION_CFG_SELECTED)
                ))?;
            }
            self.run(&format!(
                "MATCH ()-[r:{rel}]->() WHERE r.resolution_method = {} DELETE r",
                cypher_str(RESOLUTION_CFG_SELECTED)
            ))?;
        }
        Ok(())
    }

    /// `id -> cfg_active` of every twin node, normalized. Read-only: a label
    /// without the column contributes `unknown` for its twins.
    pub fn cfg_active_by_id(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for label in CFG_GATE_LABELS {
            let has_column = self
                .node_column_exists(label, "cfg_active")
                .unwrap_or(false);
            let column = if has_column { "n.cfg_active" } else { "''" };
            let cypher = format!(
                "MATCH (n:{label}) WHERE n.id CONTAINS {} RETURN n.id, {column}",
                cypher_str(TWIN_MARK)
            );
            if let Ok(rows) = self.execute_query(&cypher) {
                for row in rows.rows {
                    out.insert(row[0].clone(), normalize(&row[1]));
                }
            }
        }
        out
    }

    /// The twins of the item named `plain` (a qualified name without suffixes):
    /// every node whose id gives `plain` once the suffixes are stripped, by id.
    /// Empty when the item has no twin. Read-only.
    pub fn cfg_twin_rows(&self, plain: &str) -> Vec<TwinRow> {
        let leaf = plain.rsplit("::").next().unwrap_or(plain);
        let mut out = Vec::new();
        for label in CFG_GATE_LABELS {
            let has_active = self
                .node_column_exists(label, "cfg_active")
                .unwrap_or(false);
            let active = if has_active { "n.cfg_active" } else { "''" };
            let cypher = format!(
                "MATCH (n:{label}) WHERE n.name = {} AND n.id CONTAINS {} \
                 RETURN n.id, n.cfg_gate, {active}",
                cypher_str(leaf),
                cypher_str(TWIN_MARK)
            );
            let Ok(rows) = self.execute_query(&cypher) else {
                continue;
            };
            for row in rows.rows {
                if strip_cfg_gates(&row[0]) == plain {
                    out.push(TwinRow {
                        cfg_gate: display_gate(&row[1], &row[0]),
                        cfg_active: normalize(&row[2]),
                        id: row[0].clone(),
                        label: label.to_string(),
                    });
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Counts for `index_status`. Read-only.
    pub fn cfg_twin_summary(&self) -> CfgTwinSummary {
        let active = self.cfg_active_by_id();
        let mut summary = CfgTwinSummary {
            members: active.len() as u64,
            ..CfgTwinSummary::default()
        };
        let mut by_plain: HashMap<String, u64> = HashMap::new();
        for id in active.keys() {
            *by_plain.entry(strip_cfg_gates(id)).or_default() += 1;
        }
        summary.sets = by_plain.values().filter(|n| **n >= 2).count() as u64;
        let mut inactive_ids: Vec<&String> = Vec::new();
        for (id, value) in &active {
            match value.as_str() {
                CFG_ACTIVE => summary.active += 1,
                CFG_INACTIVE => {
                    summary.inactive += 1;
                    inactive_ids.push(id);
                }
                _ => summary.unknown += 1,
            }
        }
        inactive_ids.sort();
        summary.inactive_examples = inactive_ids
            .into_iter()
            .take(CFG_TWIN_EXAMPLE_CAP)
            .map(|id| TwinRow {
                id: id.clone(),
                label: String::new(),
                cfg_gate: display_gate("", id),
                cfg_active: CFG_INACTIVE.to_string(),
            })
            .collect();
        summary.unresolved_sites = self.count_cfg_twin_sites();
        summary
    }

    fn count_cfg_twin_sites(&self) -> u64 {
        if !self
            .node_column_exists("CallSite", "unresolved_reason")
            .unwrap_or(false)
        {
            return 0;
        }
        let cypher = format!(
            "MATCH (c:CallSite) WHERE c.is_resolved = false AND c.unresolved_reason = {} \
             RETURN count(c)",
            cypher_str(super::CALLSITE_UNRESOLVED_REASON_CFG_TWINS)
        );
        self.execute_query(&cypher)
            .ok()
            .and_then(|q| q.rows.first().and_then(|r| r.first()?.parse().ok()))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gates_of_an_id_are_read_in_path_order() {
        assert_eq!(
            cfg_gates_in("src/lib.rs::m#cfg(unix)::f#cfg(all(a,b))"),
            ["unix", "all(a,b)"]
        );
        assert!(cfg_gates_in("src/lib.rs::f").is_empty());
        assert!(cfg_gates_in("src/lib.rs::f#cfg(unclosed").is_empty());
    }

    #[test]
    fn a_container_member_shows_the_gates_of_its_path_as_one_conjunction() {
        assert_eq!(display_gate("", "src/lib.rs::m#cfg(unix)::f"), "unix");
        assert_eq!(
            display_gate("", "src/lib.rs::m#cfg(unix)::S#cfg(kani)::f"),
            "all(unix,kani)"
        );
        assert_eq!(
            display_gate("not(unix)", "src/lib.rs::f#cfg(not(unix))"),
            "not(unix)"
        );
    }

    #[test]
    fn only_the_two_decided_values_survive_normalization() {
        assert_eq!(normalize("active"), "active");
        assert_eq!(normalize("inactive"), "inactive");
        assert_eq!(normalize(""), "unknown");
        assert_eq!(normalize("maybe"), "unknown");
    }
}
