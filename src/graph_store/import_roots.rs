// import_roots: promote the receiver hints that need crate evidence (issues
// #348 and #349).
//
// The parser reads a free function's return type from its own file. When the
// name is shown only by an explicit `use` of a path outside `crate`, `self` and
// `super` (`use dy_wcet::TaskSet;`), the file cannot say whether that path
// names a crate of THIS repository or a foreign one, and the lookup by last
// segment would match any repository type of that name. The parser therefore
// marks the hint `return-type-import:<first segment>`. The indexer, which runs
// `cargo metadata` anyway, knows the library crates of the workspace and calls
// `verify_repo_crate_roots`: a hint whose first segment is one of them becomes
// a plain `return-type` hint; every other one stays unverified and the
// resolver declines it. Absent evidence (no Cargo.toml, cargo missing, an old
// graph) is therefore a declined hint, never an accepted one.

use std::collections::BTreeSet;

use super::{
    cypher_str, GraphStore, RECEIVER_HINT_VIA_IMPORT_PREFIX, RECEIVER_HINT_VIA_RETURN_TYPE,
};

impl GraphStore {
    /// Turns each `return-type-import:<root>` hint whose root is in
    /// `crate_names` into a verified `return-type` hint. Returns the number of
    /// distinct roots promoted.
    pub fn verify_repo_crate_roots(&self, crate_names: &BTreeSet<String>) -> Result<u64, String> {
        self.ensure_node_column("CallSite", "receiver_hint_via", "STRING DEFAULT ''")?;
        let pending = self.execute_query(&format!(
            "MATCH (cs:CallSite) WHERE cs.receiver_hint_via STARTS WITH {} \
             RETURN DISTINCT cs.receiver_hint_via",
            cypher_str(RECEIVER_HINT_VIA_IMPORT_PREFIX)
        ))?;
        let mut promoted = 0_u64;
        for row in &pending.rows {
            let via = &row[0];
            let root = via
                .strip_prefix(RECEIVER_HINT_VIA_IMPORT_PREFIX)
                .unwrap_or_default();
            if !crate_names.contains(root) {
                continue;
            }
            self.run(&format!(
                "MATCH (cs:CallSite) WHERE cs.receiver_hint_via = {} \
                 SET cs.receiver_hint_via = {}",
                cypher_str(via),
                cypher_str(RECEIVER_HINT_VIA_RETURN_TYPE)
            ))?;
            promoted += 1;
        }
        Ok(promoted)
    }
}
