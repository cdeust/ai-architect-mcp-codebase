// graph_store::membership — the two clustering-membership traversals the read
// side shares.
//
// `MemberOf_<Label>_Community` and `ParticipatesIn_<Label>_Process` were each
// spelled out at both ends of the pipeline: `search` enriches every ranked hit
// with them, and `clustering::impact` collects them for a blast radius. Two
// copies of one traversal is how the escaping of a file-path-bearing key comes
// to be fixed on one and missed on the other (fleet-watch#16 was exactly that,
// on the search copy).
//
// They live here, beside the schema that declares the tables, because both
// callers already depend on `graph_store` and neither depends on the other.
// The two call sites differ only in how they identify the starting symbol,
// which is what [`SymbolMatch`] names.

use super::{cypher_str, GraphStore};

/// How a membership traversal identifies the symbol it starts from.
#[derive(Clone, Copy)]
pub enum SymbolMatch<'a> {
    /// Match `n.id` against this RAW value. It is escaped here — callers on
    /// this arm hold a `qualified_name`, which embeds a file path and is
    /// therefore attacker-influenced (fleet-watch#16).
    Id(&'a str),
    /// Match `n.id` OR `n.qualified_name` against this ALREADY-escaped Cypher
    /// literal, the form `clustering::impact` threads through its helpers.
    EscapedIdOrQualifiedName(&'a str),
}

impl SymbolMatch<'_> {
    /// The `WHERE` predicate this match contributes, with `n` bound.
    fn predicate(&self) -> String {
        match self {
            SymbolMatch::Id(raw) => format!("n.id = {}", cypher_str(raw)),
            SymbolMatch::EscapedIdOrQualifiedName(escaped) => {
                format!("n.id = {escaped} OR n.qualified_name = {escaped}")
            }
        }
    }
}

/// Community ids the symbol belongs to under `label`, via
/// `MemberOf_<label>_Community`.
///
/// Clustering assigns each node to exactly one community per run
/// (`community_persist` emits one `MemberOf` edge per node index), so this is
/// a single id in a well-formed graph; it returns a list rather than an
/// `Option` because a graph clustered twice at different resolutions carries
/// both, and silently keeping one of them would be a lie about the data.
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

/// Process names the symbol participates in under `label`, via
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

/// First column of every row, dropping a failed query to an empty list — a
/// membership traversal against a label whose table a older graph never
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
