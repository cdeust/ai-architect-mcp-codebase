// graph_store::membership — the two clustering-membership traversals the read
// side shares.
//
// `MemberOf_<Label>_Community` and `ParticipatesIn_<Label>_Process` were each
// spelled out at both ends of the pipeline, and in three further places
// besides: `search` enriching every ranked hit, `search::context` answering
// get_context, `clustering::impact` collecting a blast radius,
// `security_gates` classifying an auth community, and `prd_validator`'s two
// axes. Five copies of one traversal is how the escaping of a file-path-bearing
// key comes to be fixed on one and missed on the others — fleet-watch#16 was
// exactly that, on the search copy.
//
// They live here, beside the schema that declares the tables, because every
// caller already depends on `graph_store` and none depends on the others.
//
// ESCAPING IS THIS MODULE'S JOB, NOT THE CALLER'S. Every `SymbolMatch` arm
// takes a RAW value and escapes it here. An earlier revision of this module
// took an already-escaped literal on one arm, guarded only by a doc comment;
// that hands the fleet-watch#16 injection class straight back to a call site
// where nothing checks it and the compiler cannot tell the two apart. A raw
// `&str` that must already be quoted is not an API, it is a trap.

use super::{cypher_str, GraphStore};

/// How a membership traversal identifies the symbol it starts from.
///
/// Every arm carries a RAW value; this module escapes it.
#[derive(Clone, Copy)]
pub enum SymbolMatch<'a> {
    /// Match on `n.id` alone.
    Id(&'a str),
    /// Match on `n.qualified_name` alone.
    QualifiedName(&'a str),
    /// Match on either key — the forgiving form, for callers that accept both
    /// a stored id and a qualified name for the same target.
    IdOrQualifiedName(&'a str),
}

impl SymbolMatch<'_> {
    /// The `WHERE` predicate this match contributes, with `n` bound. The value
    /// is escaped here, on every arm.
    fn predicate(&self) -> String {
        match *self {
            SymbolMatch::Id(raw) => format!("n.id = {}", cypher_str(raw)),
            SymbolMatch::QualifiedName(raw) => {
                format!("n.qualified_name = {}", cypher_str(raw))
            }
            SymbolMatch::IdOrQualifiedName(raw) => {
                let escaped = cypher_str(raw);
                format!("n.id = {escaped} OR n.qualified_name = {escaped}")
            }
        }
    }
}

/// A `Community` node's row, as the read side consumes it.
pub struct CommunityRow {
    pub id: String,
    pub name: String,
    pub member_count: u64,
}

/// The community `symbol` belongs to under `label`, via
/// `MemberOf_<label>_Community`.
///
/// `LIMIT 1` is load-bearing rather than cosmetic: this runs once per candidate
/// on the substring fallback, which scans every node of every searchable label,
/// so an uncapped probe materializes each node's whole `MemberOf` row set on
/// the hottest read path. Clustering writes one `MemberOf` edge per node per
/// run (`community_persist`), so one row is also the whole answer for a graph
/// clustered once; a graph clustered twice at two resolutions carries both, and
/// [`community_ids`] is the call for wanting all of them.
pub fn community_of(
    store: &GraphStore,
    label: &str,
    symbol: SymbolMatch<'_>,
) -> Option<CommunityRow> {
    let rel = format!("MemberOf_{label}_Community");
    let cypher = format!(
        "MATCH (n:{label})-[:{rel}]->(c:Community) WHERE {} \
         RETURN c.id, c.name, c.member_count LIMIT 1",
        symbol.predicate()
    );
    let qr = store.execute_query(&cypher).ok()?;
    let row = qr.rows.first()?;
    if row.len() < 3 {
        return None;
    }
    Some(CommunityRow {
        id: row[0].clone(),
        name: row[1].clone(),
        member_count: row[2].parse::<u64>().unwrap_or(0),
    })
}

/// Every community `symbol` belongs to under `label`. Uncapped, for
/// blast-radius collection; the per-hit read paths want [`community_of`].
pub fn community_ids(store: &GraphStore, label: &str, symbol: SymbolMatch<'_>) -> Vec<String> {
    let rel = format!("MemberOf_{label}_Community");
    first_column(
        store,
        &format!(
            "MATCH (n:{label})-[:{rel}]->(c:Community) WHERE {} RETURN c.id",
            symbol.predicate()
        ),
    )
}

/// Process names `symbol` participates in under `label`, via
/// `ParticipatesIn_<label>_Process`.
///
/// Only `Function` and `Method` carry that table; every other label yields an
/// empty list without touching the store, which is why callers need no label
/// guard of their own.
pub fn process_names(store: &GraphStore, label: &str, symbol: SymbolMatch<'_>) -> Vec<String> {
    if !matches!(label, "Function" | "Method") {
        return Vec::new();
    }
    let rel = format!("ParticipatesIn_{label}_Process");
    first_column(
        store,
        &format!(
            "MATCH (n:{label})-[:{rel}]->(p:Process) WHERE {} RETURN p.name",
            symbol.predicate()
        ),
    )
}

/// First column of every row, mapping a failed query to an empty list — a
/// membership traversal against a label whose table an older graph never
/// created is a miss, not an error the caller can act on.
fn first_column(store: &GraphStore, cypher: &str) -> Vec<String> {
    match store.execute_query(cypher) {
        Ok(qr) => qr
            .rows
            .iter()
            .filter_map(|row| row.first().cloned())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fleet-watch#16 regression guard, at the layer that now owns escaping.
    /// A qualified name embeds a file path, so an adversarially-named indexed
    /// file reaches this predicate; every arm must quote and escape it rather
    /// than interpolate it raw.
    #[test]
    fn every_symbol_match_arm_escapes_its_raw_value() {
        let hostile = "a.rs::x' OR 1=1 --";
        for symbol in [
            SymbolMatch::Id(hostile),
            SymbolMatch::QualifiedName(hostile),
            SymbolMatch::IdOrQualifiedName(hostile),
        ] {
            let predicate = symbol.predicate();
            assert!(
                predicate.contains("\\'"),
                "the embedded quote must be backslash-escaped: {predicate}"
            );
            assert!(
                !predicate.contains("= a.rs"),
                "the value must be quoted, not bare: {predicate}"
            );
        }
    }
}
