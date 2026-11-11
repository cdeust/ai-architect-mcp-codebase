// resolver::retract_edges: the import, use, implements and extends rows a
// pass no longer decides (issue #443), the twin of `retract` for the rows
// that are not calls.
//
// Every phase decides every reference again on every pass, but the
// `EdgeBuffer` only adds rows, and `resolve_imports` only ever sets an
// Import's `is_resolved` to true. Two cases a full index of the same tree does
// not have survived an incremental refresh:
//   - a File node is kept when its file is edited, so the `Imports_File_*`
//     rows of an import the edit removed stayed (`use crate::model::Widget;`
//     deleted from `user.rs`, the row `user.rs -> model.rs::Widget` remained);
//   - an Import of an unchanged file whose target an edit removed, a deletion
//     took away or an added file made ambiguous kept `is_resolved = true`, and
//     in the ambiguous case its row.
//
// Sequence, around the pass (driven by `retract::Retraction`):
//   1. before it, read the rows this module owns (`load_owned_rows`) and set
//      every Import back to unresolved (`reopen_imports`); the import phase
//      sets back to resolved each one it decides;
//   2. after the flush, delete the owned rows the pass did not meet again
//      (`retract_unmet`).
// A crash between 1 and 2 leaves Imports open whose rows still exist: the safe
// side, which the next resolve repairs.
//
// Owned: a row of an `Imports_*`, `Uses_*`, `Implements_*` or `Extends_*`
// table whose `resolution_method` is one the resolver's phases write (the
// static tier labels, which the import phase and the call phase's `Uses_*`
// rows carry, and the labels of the use, implements and extends phases). Rows
// of other writers keep their own life cycle: the light links (`js-regex`),
// the IaC pass, the language server, the macro layer.

use super::retract::{static_call_methods, RowKey};
use crate::graph_store::{cypher_str, GraphStore, REL_TABLES};
use std::collections::{BTreeSet, HashSet};

/// `resolution_method` of a `Uses_*` row from a callable's return type or a
/// type it constructs (`uses::resolve_callable_type_uses`).
pub(super) const METHOD_TYPE_REFERENCE: &str = "type-reference-parse";
/// `resolution_method` of a `Uses_*` row from a field's type annotation.
pub(super) const METHOD_TYPE_ANNOTATION: &str = "type-annotation-parse";
/// `resolution_method` of an `Implements_*` row from a declared conformance.
pub(super) const METHOD_DECLARED_IMPLEMENTS: &str = "declared-implements";
/// `resolution_method` of an `Implements_*` row from a derive or decorator.
pub(super) const METHOD_DERIVE_MACRO: &str = "derive-macro";
/// `resolution_method` of an `Implements_*` row from an `impl Trait for T`.
pub(super) const METHOD_IMPL_BLOCK: &str = "impl-block";
/// `resolution_method` of an `Extends_*` row from a declared base.
pub(super) const METHOD_DECLARED_EXTENDS: &str = "declared-extends";

/// The table prefixes this module owns rows in.
const OWNED_PREFIXES: [&str; 4] = ["Imports_", "Uses_", "Implements_", "Extends_"];

fn owned_tables() -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str)> {
    REL_TABLES
        .iter()
        .filter(|(rel, _, _)| OWNED_PREFIXES.iter().any(|p| rel.starts_with(p)))
}

/// The quoted `resolution_method` labels of the rows owned here.
fn method_list() -> String {
    static_call_methods()
        .into_iter()
        .chain([
            METHOD_TYPE_REFERENCE,
            METHOD_TYPE_ANNOTATION,
            METHOD_DECLARED_IMPLEMENTS,
            METHOD_DERIVE_MACRO,
            METHOD_IMPL_BLOCK,
            METHOD_DECLARED_EXTENDS,
        ])
        .map(cypher_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The owned rows in the graph before the pass runs.
pub(super) fn load_owned_rows(store: &GraphStore) -> HashSet<RowKey> {
    let methods = method_list();
    let mut rows = HashSet::new();
    for &(rel, from, to) in owned_tables() {
        let cypher = format!(
            "MATCH (a:{from})-[r:{rel}]->(b:{to}) WHERE r.resolution_method IN [{methods}] \
             RETURN a.id, b.id"
        );
        // A table an older graph lacks has no row to retract.
        let Ok(qr) = store.execute_query(&cypher) else {
            continue;
        };
        for row in qr.rows {
            if row.len() >= 2 {
                rows.insert((rel.to_string(), row[0].clone(), row[1].clone()));
            }
        }
    }
    rows
}

/// Step 1: sets `is_resolved = false` on every Import that holds true, so the
/// import phase that follows is the only source of the flag.
pub(super) fn reopen_imports(store: &GraphStore) -> Result<(), String> {
    store.execute_query("MATCH (i:Import) WHERE i.is_resolved = true SET i.is_resolved = false")?;
    Ok(())
}

/// Step 2: deletes the rows of `owned` absent from `met` (the persisted rows
/// the pass decided again). Returns the number of rows deleted.
///
/// Precondition: `owned` was read by `load_owned_rows` before the pass, and
/// the pass's buffer has been flushed. Idempotent: a second call with the same
/// arguments finds nothing left to delete.
pub(super) fn retract_unmet(
    store: &GraphStore,
    owned: &HashSet<RowKey>,
    met: &HashSet<RowKey>,
) -> Result<u64, String> {
    let stale: BTreeSet<&RowKey> = owned.difference(met).collect();
    if stale.is_empty() {
        return Ok(0);
    }
    let methods = method_list();
    let mut deleted = 0u64;
    for (rel, from_id, to_id) in stale {
        let Some(&(_, from, to)) = owned_tables().find(|(t, _, _)| t == rel) else {
            continue;
        };
        store.execute_query(&format!(
            "MATCH (a:{from})-[r:{rel}]->(b:{to}) WHERE a.id = {} AND b.id = {} \
             AND r.resolution_method IN [{methods}] DELETE r",
            cypher_str(from_id),
            cypher_str(to_id),
        ))?;
        deleted += 1;
    }
    Ok(deleted)
}

#[cfg(test)]
#[path = "retract_edges_tests.rs"]
mod tests;
