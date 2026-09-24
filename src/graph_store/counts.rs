// counts — what "edge count" means for a graph (issue #338).
//
// Layer: persistence. One definition, computed in one pass over `REL_TABLES`,
// so every surface that reports node/edge totals answers the same question.
//
// A relationship row is a graph edge when it states a fact of its own. The
// per-site tables `Calls_CallSite_*` (issue #335) restate a fact the symbol
// level `Calls_<Caller>_<Target>` edge already records, at the granularity of
// one call site, so counting them would report growth that is not new
// information: a graph written before #335 and one written after it, over the
// same code, must report the same `edge_count`. They are reported as their
// own figure, `call_site_targets`.
//
// The class is derived from the table's shape (its source label and its
// `Calls_` prefix), never from a list of table names, so a per-site table
// added later classifies itself. `call_site_rel_table` is the writer side of
// the same rule; the unit tests tie the two together.

use super::schema::{NODE_CALL_SITE, REL_TABLES};
use super::serialize::value_to_u64;
use super::GraphStore;

/// How a relationship table is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelCountClass {
    /// States a fact of its own: counted in `edge_count`.
    GraphEdge,
    /// Restates, per call site, a resolution the symbol-level `Calls_*` edge
    /// records: counted in `call_site_targets` only.
    CallSiteTarget,
}

/// Classifies the relationship table `rel` whose source node label is
/// `from_label`. `Defines_Function_CallSite` and `Defines_Method_CallSite`
/// have a `CallSite` as target, not as source, and stay graph edges: the
/// `CallSite` nodes they anchor are counted as nodes.
pub fn rel_count_class(rel: &str, from_label: &str) -> RelCountClass {
    if from_label == NODE_CALL_SITE && rel.starts_with("Calls_") {
        RelCountClass::CallSiteTarget
    } else {
        RelCountClass::GraphEdge
    }
}

/// Node and relationship totals of one graph, split by `RelCountClass`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphCounts {
    pub nodes: u64,
    /// Rows in tables classified `GraphEdge`.
    pub edges: u64,
    /// Rows in tables classified `CallSiteTarget`.
    pub call_site_targets: u64,
}

impl GraphStore {
    /// Number of graph edges: relationship rows that state a fact of their
    /// own (see the module comment). Per-site `Calls_CallSite_*` rows are
    /// not included; `graph_counts` reports them separately.
    pub fn edge_count(&self) -> Result<u64, String> {
        Ok(self.rel_row_counts()?.0)
    }

    /// Node total and both relationship figures, from one pass over the
    /// relationship tables.
    pub fn graph_counts(&self) -> Result<GraphCounts, String> {
        let (edges, call_site_targets) = self.rel_row_counts()?;
        Ok(GraphCounts {
            nodes: self.node_count()?,
            edges,
            call_site_targets,
        })
    }

    /// `(graph edges, per-site target rows)`. A table that cannot be queried
    /// counts as empty, as `edge_count` always has: a graph written by an older
    /// schema legitimately lacks some tables, so a failed count cannot be told
    /// from an absent table and the totals are a floor on a damaged graph.
    fn rel_row_counts(&self) -> Result<(u64, u64), String> {
        let (mut edges, mut targets) = (0_u64, 0_u64);
        for &(rel, from, _) in REL_TABLES {
            let cypher = format!("MATCH ()-[r:{rel}]->() RETURN count(r)");
            let Ok(mut result) = self.run(&cypher) else {
                continue;
            };
            let Some(row) = result.next() else {
                continue;
            };
            let rows = value_to_u64(&row[0]);
            match rel_count_class(rel, from) {
                RelCountClass::GraphEdge => edges += rows,
                RelCountClass::CallSiteTarget => targets += rows,
            }
        }
        Ok((edges, targets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_store::schema::{
        call_site_rel_table, NODE_FUNCTION, NODE_METHOD, NODE_STDLIB_SYMBOL,
    };

    /// The writer side (`call_site_rel_table`) and the counter must agree on
    /// which tables are per-site rows, in both directions, so a table added
    /// to one side cannot be counted as an edge by the other.
    #[test]
    fn per_site_tables_written_and_per_site_tables_counted_are_the_same_set() {
        let mut written: Vec<&str> = [NODE_FUNCTION, NODE_METHOD, NODE_STDLIB_SYMBOL]
            .into_iter()
            .filter_map(call_site_rel_table)
            .collect();
        let mut counted: Vec<&str> = REL_TABLES
            .iter()
            .filter(|&&(rel, from, _)| rel_count_class(rel, from) == RelCountClass::CallSiteTarget)
            .map(|&(rel, _, _)| rel)
            .collect();
        written.sort_unstable();
        counted.sort_unstable();
        assert_eq!(written, counted);
    }

    /// Every table the writer can target is declared with a `CallSite`
    /// source, so the shape rule reaches it.
    #[test]
    fn every_written_per_site_table_is_declared_with_a_call_site_source() {
        for target in [NODE_FUNCTION, NODE_METHOD, NODE_STDLIB_SYMBOL] {
            let rel = call_site_rel_table(target).expect("a per-site table");
            let declared = REL_TABLES.iter().find(|&&(name, _, _)| name == rel);
            let &(_, from, to) = declared.expect("declared in REL_TABLES");
            assert_eq!((from, to), (NODE_CALL_SITE, target), "{rel}");
        }
    }

    /// A table that has a `CallSite` as target keeps counting as an edge.
    #[test]
    fn a_relation_into_a_call_site_is_a_graph_edge() {
        assert_eq!(
            rel_count_class("Defines_Function_CallSite", "Function"),
            RelCountClass::GraphEdge
        );
        assert_eq!(
            rel_count_class("Defines_Method_CallSite", "Method"),
            RelCountClass::GraphEdge
        );
    }
}
