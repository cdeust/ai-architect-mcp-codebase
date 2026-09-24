// macro_reset — undo what a previous macro-expansion pass wrote (issue #339).
//
// The macro layer derives its rows from the CallSite nodes alone, so a run
// re-derives all of them. Rows written by an older build can name targets the
// current rules would not (`writeln!` on an `io::Write` also pointing at
// `fmt::Write::write_fmt`), and `EdgeBuffer` treats an existing row as already
// resolved, so they would survive every re-run. Clearing them first makes the
// graph a function of the current rules and the source, not of what ran before.

use super::cypher_str;
use super::schema::{NODE_STDLIB_SYMBOL, REL_TABLES};
use super::GraphStore;

// source: resolver_layers — every macro-expansion method label starts with it.
const MACRO_METHOD_PREFIX: &str = "macro-expansion";

impl GraphStore {
    /// Deletes every macro-expansion row of every `Calls_*_StdlibSymbol`
    /// table, and clears `is_resolved` on every macro-marker `CallSite`
    /// (`callee_name` ending in `!`) so the pass decides it again.
    pub(crate) fn reset_macro_expansion(&self) -> Result<(), String> {
        self.ensure_node_column("CallSite", "is_resolved", "BOOLEAN DEFAULT false")?;
        for &(rel, _, to) in REL_TABLES {
            if to != NODE_STDLIB_SYMBOL || !rel.starts_with("Calls_") {
                continue;
            }
            self.run(&format!(
                "MATCH ()-[r:{rel}]->() WHERE r.resolution_method STARTS WITH {} DELETE r",
                cypher_str(MACRO_METHOD_PREFIX)
            ))?;
        }
        self.run(
            "MATCH (cs:CallSite) WHERE cs.callee_name ENDS WITH '!' SET cs.is_resolved = false",
        )?;
        Ok(())
    }
}
