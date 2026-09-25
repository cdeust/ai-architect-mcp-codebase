// graph_store::cfg_twins: what a `#cfg(..)` id suffix means to the graph layer
// (issue #353).
//
// Two items of a name under mutually exclusive `#[cfg]` predicates are two nodes.
// The parser names each by appending `#cfg(<compact gate>)` to its qualified
// name (`src/lib.rs::pick#cfg(not(feature=fast))`); a member of a twin module
// carries the suffix in the middle of its name (`src/lib.rs::m#cfg(unix)::f`).
// This module is the one place that reads that shape back, so the resolver, the
// impact tools and the status counters cannot disagree on it.

use super::{cypher_str, GraphStore};

/// Opens the suffix of a twin id. Mirrors `parser::spec::rust_cfg_gate::TWIN_MARK`
/// (a test below keeps the two equal): the graph layer does not import the parser.
pub(crate) const TWIN_MARK: &str = "#cfg(";

/// Attribution of a `CallSite` left unresolved on purpose: every candidate for
/// its callee is one of several twins of one item, so no single edge is honest.
/// Written by the static resolver (`resolver::calls`); the impact tools and
/// `index_status` read it in the next change. `''` still means "not attributed".
/// source: issue #353.
pub const CALLSITE_UNRESOLVED_REASON_CFG_TWINS: &str = "cfg_twins";

/// The node tables that carry the `cfg_gate` column, exactly those of the DDL.
///
/// The presence of the column proves nothing by itself, because the ids of the
/// twins depend on the canonical form of the gate (comment stripping, string
/// escapes, ordering), and that form changed during this work. What proves a
/// graph carries current twin identity is the marker row of
/// `CANONICAL_FORM_VERSION` in the `GraphMarker` table, written only by the
/// indexer of a build that produces that form. `require_cfg_gate_metadata`
/// demands both: every column and the current marker. The column is created only
/// by `create_schema` (a test scans the sources for an `ALTER`), and a full
/// reindex removes the old graph directory first
/// (`indexing_handlers::do_index_codebase`), so the tables and the marker are
/// rebuilt together. Bump `CANONICAL_FORM_VERSION` whenever the canonical form or
/// the compact text of a gate changes.
pub const CFG_GATE_LABELS: [&str; 10] = [
    "Module",
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Variant",
    "Trait",
    "Field",
    "Constant",
    "TypeAlias",
];

/// True when `qn` carries a twin suffix anywhere in its path.
pub(crate) fn has_cfg_gate(qn: &str) -> bool {
    qn.contains(TWIN_MARK)
}

/// `qn` without its twin suffixes: the name every twin of an item shares. A
/// suffix that never closes is left alone, so an odd id is never mangled.
pub(crate) fn strip_cfg_gates(qn: &str) -> String {
    let mut out = String::with_capacity(qn.len());
    let mut rest = qn;
    while let Some(start) = rest.find(TWIN_MARK) {
        let after = &rest[start + TWIN_MARK.len()..];
        let Some(close) = closing_paren(after) else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The index of the `)` that closes a group already opened, in `text`.
fn closing_paren(text: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// The version of the canonical form of a `#[cfg]` gate, as written into every
/// id of a twin. Version 2: comments removed before the predicate is read and
/// string escapes decoded. A graph with a lower version, or none, holds twin ids
/// that a current build would spell differently.
/// source: issue #353. The pair (version, golden digest) is pinned by
/// `parser::spec::rust_cfg_canonical_tests`: changing a golden spelling fails it
/// until this version is bumped with it.
pub const CANONICAL_FORM_VERSION: u32 = 2;

/// The table that holds the marker rows; created by `create_schema`.
pub(crate) const MARKER_TABLE: &str = "GraphMarker";
const MARKER_ID: &str = "cfg_canonical_form";

impl GraphStore {
    /// True when node table `label` exists.
    pub fn has_node_table(&self, label: &str) -> Result<bool, String> {
        let tables = self.execute_query("CALL show_tables() RETURN *")?;
        Ok(tables
            .rows
            .iter()
            .any(|row| row.get(1).is_some_and(|name| name == label)))
    }

    /// Records that this graph is being written with the current canonical form.
    pub fn write_canonical_marker(&self) -> Result<(), String> {
        self.execute_query(&format!(
            "MERGE (m:{MARKER_TABLE} {{id: {}}}) SET m.value = {}",
            cypher_str(MARKER_ID),
            cypher_str(&CANONICAL_FORM_VERSION.to_string())
        ))?;
        Ok(())
    }

    /// The canonical form version the graph was written with, `None` when it has
    /// no marker (a graph from before the marker existed).
    fn canonical_marker(&self) -> Result<Option<u32>, String> {
        if !self.has_node_table(MARKER_TABLE)? {
            return Ok(None);
        }
        let rows = self.execute_query(&format!(
            "MATCH (m:{MARKER_TABLE} {{id: {}}}) RETURN m.value",
            cypher_str(MARKER_ID)
        ))?;
        Ok(rows.rows.first().and_then(|r| r[0].parse().ok()))
    }

    /// Issue #353: an old graph keeps ONE node for two items of a name under
    /// mutually exclusive `#[cfg]` predicates, and a graph from an earlier form of
    /// the gate names them differently. An empty column cannot fix either:
    /// reparse every file through the full-index handler. Read-only, like
    /// `require_entry_metadata`, so a caller can check compatibility before it
    /// mutates a graph.
    pub fn require_cfg_gate_metadata(&self) -> Result<(), String> {
        let mut complete = true;
        for label in CFG_GATE_LABELS {
            complete &= self.node_column_exists(label, "cfg_gate")?;
        }
        if complete && self.canonical_marker()? == Some(CANONICAL_FORM_VERSION) {
            Ok(())
        } else {
            Err(format!(
                "graph lacks #[cfg] twin metadata (cfg_gate, canonical form {CANONICAL_FORM_VERSION}); full reindex required (index_codebase with full: true)"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_equals_the_parsers() {
        assert_eq!(TWIN_MARK, crate::parser::CFG_TWIN_MARK);
    }

    #[test]
    fn a_leaf_suffix_is_stripped() {
        assert_eq!(
            strip_cfg_gates("src/lib.rs::pick#cfg(not(feature=fast))"),
            "src/lib.rs::pick"
        );
    }

    #[test]
    fn a_suffix_in_the_middle_of_a_path_is_stripped() {
        assert_eq!(
            strip_cfg_gates("src/lib.rs::m#cfg(unix)::f"),
            "src/lib.rs::m::f"
        );
    }

    #[test]
    fn several_suffixes_are_all_stripped() {
        assert_eq!(
            strip_cfg_gates("src/lib.rs::m#cfg(unix)::S#cfg(all(a,b))::f#cfg(kani)"),
            "src/lib.rs::m::S::f"
        );
    }

    #[test]
    fn a_name_without_a_suffix_and_an_unclosed_one_are_unchanged() {
        assert_eq!(strip_cfg_gates("src/lib.rs::pick"), "src/lib.rs::pick");
        assert_eq!(
            strip_cfg_gates("src/lib.rs::pick#cfg(x"),
            "src/lib.rs::pick#cfg(x"
        );
        assert!(!has_cfg_gate("src/lib.rs::pick"));
        assert!(has_cfg_gate("src/lib.rs::m#cfg(unix)::f"));
    }

    #[test]
    fn a_sequence_suffix_is_not_a_twin_suffix() {
        assert_eq!(strip_cfg_gates("src/lib.rs::f#3"), "src/lib.rs::f#3");
    }
}
