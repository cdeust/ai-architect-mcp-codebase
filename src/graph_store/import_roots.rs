// import_roots: the crate evidence a receiver hint needs when its type is named
// through a `use` (issues #348, #349 and #358).
//
// The parser reads a free function's return type from its own file. When the
// name is shown only by an explicit `use` of a path outside `crate`, `self` and
// `super` (`use dy_wcet::TaskSet;`), the file cannot say whether that path names
// a crate of THIS repository or a foreign one, and the lookup by last segment
// would match any repository type of that name. The parser therefore marks the
// hint `return-type-import:<first segment>` and the mark stays as written.
//
// The indexer, which runs `cargo metadata` anyway, records the library crate
// names of the workspace on every pass (`write_crate_evidence`), and the
// resolver accepts such a hint only when its first segment is one of the names
// recorded NOW. The decision is therefore a function of the current tree: an
// incremental run after a crate rename declines the hints of files it did not
// reparse exactly as a fresh full index does (issue #358). Before this, the
// indexer rewrote an accepted mark into a plain `return-type`, which no later
// run could check again.
//
// Absent evidence (no Cargo.toml, cargo missing, a graph with no recorded
// facts) is an empty set of names, so such a hint is declined, never accepted.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::cfg_twins::MARKER_TABLE;
use super::{cypher_str, GraphStore};

/// The version of the crate evidence a graph records and of the rules that
/// read it. Bump it when either changes what a graph stores.
pub(crate) const CRATE_EVIDENCE_FORM: u32 = 1;
const FORM_MARKER_ID: &str = "crate_evidence_form";
const FACTS_MARKER_ID: &str = "crate_evidence";

/// What `cargo metadata` said about the workspace on the last index pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrateEvidence {
    /// True when the Cargo map was known on that pass.
    pub known: bool,
    /// The names other code writes to import a library of the workspace.
    pub crate_names: BTreeSet<String>,
}

impl GraphStore {
    /// Records the crate evidence of this pass, replacing the previous one.
    pub fn write_crate_evidence(&self, evidence: &CrateEvidence) -> Result<(), String> {
        let json = serde_json::to_string(evidence).map_err(|e| e.to_string())?;
        self.execute_query(&format!(
            "MERGE (m:{MARKER_TABLE} {{id: {}}}) SET m.value = {}",
            cypher_str(FACTS_MARKER_ID),
            cypher_str(&json)
        ))?;
        Ok(())
    }

    /// The crate evidence recorded by the last index pass; the default (nothing
    /// known, no crate name) when none is recorded or it cannot be read.
    pub fn crate_evidence(&self) -> CrateEvidence {
        self.marker_value(FACTS_MARKER_ID)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    /// Records that this graph was completely written with the current crate
    /// evidence rules. Called at the END of a successful full index.
    pub fn write_crate_evidence_marker(&self) -> Result<(), String> {
        self.execute_query(&format!(
            "MERGE (m:{MARKER_TABLE} {{id: {}}}) SET m.value = {}",
            cypher_str(FORM_MARKER_ID),
            cypher_str(&CRATE_EVIDENCE_FORM.to_string())
        ))?;
        Ok(())
    }

    /// Issue #358: a graph written before the evidence was recorded holds hints
    /// that an earlier pass rewrote as accepted, which no later pass can check
    /// again. Only a full reparse writes them as they are. Read-only.
    pub fn require_crate_evidence_metadata(&self) -> Result<(), String> {
        let form = self
            .marker_value(FORM_MARKER_ID)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u32>().ok());
        if form == Some(CRATE_EVIDENCE_FORM) {
            Ok(())
        } else {
            Err(format!(
                "graph lacks crate evidence metadata (form {CRATE_EVIDENCE_FORM}); full reindex required (index_codebase with full: true)"
            ))
        }
    }

    fn marker_value(&self, id: &str) -> Result<Option<String>, String> {
        if !self.has_node_table(MARKER_TABLE)? {
            return Ok(None);
        }
        let rows = self.execute_query(&format!(
            "MATCH (m:{MARKER_TABLE} {{id: {}}}) RETURN m.value",
            cypher_str(id)
        ))?;
        Ok(rows.rows.first().map(|r| r[0].clone()))
    }
}
