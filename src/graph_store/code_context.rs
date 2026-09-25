// graph_store::code_context: what the graph says about test, bench and example
// code (issue #354).
//
// Two facts, kept apart because they come from different places:
//   - `Function.code_context` / `Method.code_context`: `test`, `bench` or `proof`
//     when the source of the function proves it (a test attribute, an enclosing
//     `#[cfg(test)]`). Written by the parser, so it is a pure function of the
//     file. `''` for every other function.
//   - `File.target_context`: what the Cargo package says the file is
//     (`production`, `test`, `bench`, `example`), from `cargo metadata` and the
//     module tree. Rewritten by every index pass, full or incremental, because a
//     `Cargo.toml` edit changes it without changing a file. `''` when nothing is
//     decided.
//
// A caller is non-production when EITHER says so. Nothing is decided by
// reachability: a helper that production code also calls keeps the context of
// where it lives. `unknown` (nothing decided) counts as production in the totals,
// so a caller is never hidden on missing evidence.
//
// A graph written before these columns exist is refused for incremental writes
// (`require_code_context_metadata`: the columns and the `code_context_form`
// marker row, written last by a full index like `cfg_canonical_form`), because
// an unchanged file would keep a stale `''` until a full reindex. Reads never
// refuse: `get_impact` reports the basis `absent` and every caller `unknown`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::cfg_twins::MARKER_TABLE;
use super::{cypher_str, GraphStore, BULK_BATCH_SIZE};
use lbug::{LogicalType, Value};

/// The version of the code-context columns and of the rules that fill them.
/// Bump it when either changes what a graph stores.
pub(crate) const CODE_CONTEXT_FORM: u32 = 1;
const MARKER_ID: &str = "code_context_form";
const READ_CHUNK: usize = 200;

pub const CONTEXT_PRODUCTION: &str = "production";
pub const CONTEXT_UNKNOWN: &str = "unknown";

/// Where the answer of `caller_contexts` comes from.
pub const BASIS_CARGO_AND_SOURCE: &str = "cargo+source";
pub const BASIS_SOURCE_ONLY: &str = "source_only";
pub const BASIS_ABSENT: &str = "absent";

impl GraphStore {
    /// Records that this graph was completely written with the current
    /// code-context columns and rules. Called at the END of a successful full
    /// index (the start clears every marker row).
    pub fn write_code_context_marker(&self) -> Result<(), String> {
        self.execute_query(&format!(
            "MERGE (m:{MARKER_TABLE} {{id: {}}}) SET m.value = {}",
            cypher_str(MARKER_ID),
            cypher_str(&CODE_CONTEXT_FORM.to_string())
        ))?;
        Ok(())
    }

    fn code_context_marker(&self) -> Result<Option<u32>, String> {
        if !self.has_node_table(MARKER_TABLE)? {
            return Ok(None);
        }
        let rows = self.execute_query(&format!(
            "MATCH (m:{MARKER_TABLE} {{id: {}}}) RETURN m.value",
            cypher_str(MARKER_ID)
        ))?;
        Ok(rows.rows.first().and_then(|r| r[0].parse().ok()))
    }

    fn code_context_columns_present(&self) -> Result<bool, String> {
        Ok(self.node_column_exists("Function", "code_context")?
            && self.node_column_exists("Method", "code_context")?
            && self.node_column_exists("File", "target_context")?)
    }

    /// True when the graph carries the current code-context columns and marker.
    /// Read-only; a failed check counts as absent.
    pub fn has_code_context(&self) -> bool {
        self.code_context_columns_present().unwrap_or(false)
            && self.code_context_marker().ok().flatten() == Some(CODE_CONTEXT_FORM)
    }

    /// Issue #354: an old graph has no `code_context`, so an incremental write
    /// would leave every unchanged file with a stale `''`. Only a full reparse
    /// fills it. Read-only, like `require_cfg_gate_metadata`.
    pub fn require_code_context_metadata(&self) -> Result<(), String> {
        if self.has_code_context() {
            Ok(())
        } else {
            Err(format!(
                "graph lacks code-context metadata (code_context, target_context, form {CODE_CONTEXT_FORM}); full reindex required (index_codebase with full: true)"
            ))
        }
    }

    /// Rewrites `File.target_context` for every file: the values of `contexts`
    /// (file id to context), `''` for every file not in it.
    pub fn write_target_contexts(
        &self,
        contexts: &BTreeMap<String, &'static str>,
    ) -> Result<(), String> {
        self.run("MATCH (f:File) SET f.target_context = ''")?;
        let mut by_value: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (file, value) in contexts {
            by_value.entry(value).or_default().push(file);
        }
        for (value, files) in by_value {
            let cypher = format!(
                "UNWIND $rows AS rid MATCH (f:File {{id: rid}}) SET f.target_context = {}",
                cypher_str(value)
            );
            for chunk in files.chunks(BULK_BATCH_SIZE) {
                let ids = chunk
                    .iter()
                    .map(|id| Value::String((*id).to_string()))
                    .collect();
                self.run_prepared(&cypher, Value::List(LogicalType::String, ids))?;
            }
        }
        Ok(())
    }

    /// Whether the Cargo package decided anything for the graph's files:
    /// `cargo+source` when some file has a target context, `source_only` when the
    /// columns exist but no file has one (no `cargo`, no `Cargo.toml`, a
    /// non-Rust corpus), `absent` when the graph has no code-context columns.
    pub fn code_context_basis(&self) -> &'static str {
        if !self.has_code_context() {
            return BASIS_ABSENT;
        }
        let decided = self
            .execute_query("MATCH (f:File) WHERE f.target_context <> '' RETURN f.id LIMIT 1")
            .map(|rows| !rows.rows.is_empty())
            .unwrap_or(false);
        if decided {
            BASIS_CARGO_AND_SOURCE
        } else {
            BASIS_SOURCE_ONLY
        }
    }

    /// The context of each `(label, id)` caller: `test`, `bench`, `proof` or
    /// `example` when the source or the Cargo target proves it, `production`
    /// when its file is reached by a lib, bin or build target, `unknown`
    /// otherwise. A graph without code-context columns answers `unknown` for
    /// every caller. Keyed by node id. Read-only.
    pub fn caller_contexts(&self, callers: &[(&str, &str)]) -> HashMap<String, String> {
        let unknown = || {
            callers
                .iter()
                .map(|(_, id)| (id.to_string(), CONTEXT_UNKNOWN.to_string()))
                .collect()
        };
        if !self.has_code_context() {
            return unknown();
        }
        let mut node_context: HashMap<&str, String> = HashMap::new();
        for label in ["Function", "Method"] {
            let ids: Vec<&str> = callers
                .iter()
                .filter(|(l, _)| *l == label)
                .map(|(_, id)| *id)
                .collect();
            for chunk in ids.chunks(READ_CHUNK) {
                let cypher = format!(
                    "MATCH (n:{label}) WHERE n.id IN {} RETURN n.id, n.code_context",
                    id_list(chunk)
                );
                if let Ok(rows) = self.execute_query(&cypher) {
                    for row in rows.rows {
                        if let Some(id) = chunk.iter().find(|id| **id == row[0]) {
                            node_context.insert(id, row[1].clone());
                        }
                    }
                }
            }
        }
        let files: BTreeSet<String> = callers
            .iter()
            .filter_map(|(_, id)| crate::language_provider::extract_file_prefix(id))
            .collect();
        let file_ids: Vec<&str> = files.iter().map(String::as_str).collect();
        let mut file_context: HashMap<String, String> = HashMap::new();
        for chunk in file_ids.chunks(READ_CHUNK) {
            let cypher = format!(
                "MATCH (f:File) WHERE f.id IN {} RETURN f.id, f.target_context",
                id_list(chunk)
            );
            if let Ok(rows) = self.execute_query(&cypher) {
                for row in rows.rows {
                    file_context.insert(row[0].clone(), row[1].clone());
                }
            }
        }
        callers
            .iter()
            .map(|(_, id)| {
                let own = node_context.get(id).map(String::as_str).unwrap_or("");
                let file = crate::language_provider::extract_file_prefix(id)
                    .and_then(|f| file_context.get(&f).cloned())
                    .unwrap_or_default();
                (id.to_string(), combine(own, &file))
            })
            .collect()
    }
}

/// A Cypher list literal of `ids`.
fn id_list(ids: &[&str]) -> String {
    let items: Vec<String> = ids.iter().map(|id| cypher_str(id)).collect();
    format!("[{}]", items.join(", "))
}

/// The context of a function whose own marker is `own` and whose file is
/// `file`: the source proves more than the location, so a marked function keeps
/// its marker; otherwise the file decides; an undecided file is `unknown`.
fn combine(own: &str, file: &str) -> String {
    match (own, file) {
        ("", "") => CONTEXT_UNKNOWN.to_string(),
        ("", file) => file.to_string(),
        (own, _) => own.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_proves_more_than_the_location() {
        assert_eq!(combine("test", "production"), "test");
        assert_eq!(combine("", "test"), "test");
        assert_eq!(combine("", "production"), "production");
        assert_eq!(combine("", ""), CONTEXT_UNKNOWN);
    }

    #[test]
    fn a_list_literal_quotes_every_id() {
        assert_eq!(id_list(&["a", "b'c"]), "['a', 'b\\'c']");
    }
}
