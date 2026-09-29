// graph_store::body_kind: what a callable node stands for, and who can name it
// (issue #400).
//
// Two columns on `Function` and `Method`, written by the parser through the
// indexer:
//   - `body_kind`: `body` for a definition, `prototype` for a C or C++
//     declaration with no body, `macro` for a C function-like macro. Only the C
//     walker writes a value other than `body` today.
//   - `linkage`: `internal` for a C function written `static`, which only
//     its own file can name; '' otherwise.
//
// A C call to a function defined in another file goes through a header
// prototype. Before these columns the resolver saw the prototype and the
// definition as two candidates and left the call ambiguous; it reads them now
// to drop the prototype (`resolver::calls::declarations`).
//
// Versioned like `code_context`: a full index clears the `body_kind_form`
// marker at its start and writes it at its end; an incremental write on a graph
// without the marker is refused, because an unchanged file would keep `''`.
// Reads never refuse: a graph without the columns gives no facts, and the
// resolver behaves as it did before them.

use std::collections::{HashMap, HashSet};

use super::cfg_twins::MARKER_TABLE;
use super::{cypher_str, GraphStore};

/// The version of the columns and of the rules that fill them. Bump it when
/// either changes what a graph stores.
pub(crate) const BODY_KIND_FORM: u32 = 1;
const MARKER_ID: &str = "body_kind_form";

pub const BODY_KIND_BODY: &str = "body";
pub const BODY_KIND_PROTOTYPE: &str = "prototype";
pub const LINKAGE_INTERNAL: &str = "internal";

/// The callables that are not plain definitions visible from every file.
#[derive(Debug, Default)]
pub struct CallableFacts {
    /// Node id to `body_kind`, for every callable whose kind is not `body`.
    pub non_body: HashMap<String, String>,
    /// Ids of the callables with internal linkage.
    pub internal: HashSet<String>,
}

impl GraphStore {
    /// Records that this graph was completely written with the current columns.
    /// Called at the END of a successful full index.
    pub fn write_body_kind_marker(&self) -> Result<(), String> {
        self.execute_query(&format!(
            "MERGE (m:{MARKER_TABLE} {{id: {}}}) SET m.value = {}",
            cypher_str(MARKER_ID),
            cypher_str(&BODY_KIND_FORM.to_string())
        ))?;
        Ok(())
    }

    fn body_kind_marker(&self) -> Result<Option<u32>, String> {
        if !self.has_node_table(MARKER_TABLE)? {
            return Ok(None);
        }
        let rows = self.execute_query(&format!(
            "MATCH (m:{MARKER_TABLE} {{id: {}}}) RETURN m.value",
            cypher_str(MARKER_ID)
        ))?;
        Ok(rows.rows.first().and_then(|r| r[0].parse().ok()))
    }

    fn body_kind_columns_present(&self) -> Result<bool, String> {
        Ok(self.node_column_exists("Function", "body_kind")?
            && self.node_column_exists("Method", "body_kind")?
            && self.node_column_exists("Function", "linkage")?
            && self.node_column_exists("Method", "linkage")?)
    }

    /// True when the graph carries the current columns and marker. Read-only;
    /// a failed check counts as absent.
    pub fn has_body_kind(&self) -> bool {
        self.body_kind_columns_present().unwrap_or(false)
            && self.body_kind_marker().ok().flatten() == Some(BODY_KIND_FORM)
    }

    /// Refuses an incremental write on a graph whose unchanged files would keep
    /// a stale `''`. Only a full reparse fills the columns.
    pub fn require_body_kind_metadata(&self) -> Result<(), String> {
        if self.has_body_kind() {
            Ok(())
        } else {
            Err(format!(
                "graph lacks callable body metadata (body_kind, linkage, form {BODY_KIND_FORM}); full reindex required (index_codebase with full: true)"
            ))
        }
    }

    /// The callables that are not plain definitions visible from every file.
    /// Empty on a graph written before the columns (no error).
    pub fn callable_facts(&self) -> CallableFacts {
        let mut facts = CallableFacts::default();
        if !self.body_kind_columns_present().unwrap_or(false) {
            return facts;
        }
        for label in ["Function", "Method"] {
            let q = format!(
                "MATCH (n:{label}) WHERE (n.body_kind <> '' AND n.body_kind <> {}) \
                 OR n.linkage = {} RETURN n.id, n.body_kind, n.linkage",
                cypher_str(BODY_KIND_BODY),
                cypher_str(LINKAGE_INTERNAL)
            );
            let rows = self.execute_query(&q).map(|r| r.rows).unwrap_or_default();
            rows.iter()
                .filter(|r| r.len() == 3)
                .for_each(|r| facts.absorb(r));
        }
        facts
    }
}

impl CallableFacts {
    /// Records one `(id, body_kind, linkage)` row.
    fn absorb(&mut self, row: &[String]) {
        if !row[1].is_empty() && row[1] != BODY_KIND_BODY {
            self.non_body.insert(row[0].clone(), row[1].clone());
        }
        if row[2] == LINKAGE_INTERNAL {
            self.internal.insert(row[0].clone());
        }
    }
}
