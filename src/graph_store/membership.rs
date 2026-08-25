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

use super::GraphStore;
use lbug::Value;

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
    /// The `WHERE` predicate this match contributes, with `n` bound and the
    /// value referenced as the parameter `$v` — never interpolated. The
    /// predicate text is therefore a constant per arm, which is also what lets
    /// the prepared-statement cache hit across every call.
    fn predicate(&self) -> &'static str {
        match *self {
            SymbolMatch::Id(_) => "n.id = $v",
            SymbolMatch::QualifiedName(_) => "n.qualified_name = $v",
            SymbolMatch::IdOrQualifiedName(_) => "n.id = $v OR n.qualified_name = $v",
        }
    }

    /// The raw value this match binds to `$v`.
    fn value(&self) -> &str {
        match *self {
            SymbolMatch::Id(raw)
            | SymbolMatch::QualifiedName(raw)
            | SymbolMatch::IdOrQualifiedName(raw) => raw,
        }
    }

    /// The single `$v` binding, ready for `query_prepared_params`.
    fn params(&self) -> Vec<(&'static str, Value)> {
        vec![("v", Value::String(self.value().to_string()))]
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
/// A row whose `Community.id` is empty is reported as `None`: it identifies no
/// community, and every consumer wants that same answer.
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
    let qr = store.query_prepared_params(&cypher, symbol.params()).ok()?;
    let row = qr.rows.first()?;
    if row.len() < 3 {
        return None;
    }
    // A degenerate empty id is NOT an answer, and that belongs HERE rather than
    // at each caller. It was copy-pasted to three of the four consumers and
    // missed on the fourth (`search::context::find_community`), so the codebase
    // held two definitions of "no community" at once. One traversal, one rule.
    if row[0].is_empty() {
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
        symbol,
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
        symbol,
    )
}

/// First column of every row, mapping a failed query to an empty list — a
/// membership traversal against a label whose table an older graph never
/// created is a miss, not an error the caller can act on.
fn first_column(store: &GraphStore, cypher: &str, symbol: SymbolMatch<'_>) -> Vec<String> {
    match store.query_prepared_params(cypher, symbol.params()) {
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

    /// fleet-watch#16 regression guard, strengthened: the value is no longer
    /// escaped INTO the query, it is bound beside it. A qualified name embeds a
    /// file path, so an adversarially-named indexed file reaches this
    /// predicate; no arm may place it in the query text at all.
    #[test]
    fn no_symbol_match_arm_puts_its_value_in_the_query_text() {
        let hostile = "a.rs::x' OR 1=1 --";
        for symbol in [
            SymbolMatch::Id(hostile),
            SymbolMatch::QualifiedName(hostile),
            SymbolMatch::IdOrQualifiedName(hostile),
        ] {
            let predicate = symbol.predicate();
            assert!(
                !predicate.contains("a.rs"),
                "the value must not appear in the text at all: {predicate}"
            );
            assert!(
                predicate.contains("$v"),
                "the value must be referenced as a bound parameter: {predicate}"
            );
            let params = symbol.params();
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].0, "v");
        }
    }

    /// The predicate text is constant per arm, which is what lets one prepared
    /// statement serve every call instead of one plan per distinct value.
    #[test]
    fn the_predicate_text_does_not_vary_with_the_value() {
        assert_eq!(
            SymbolMatch::Id("a").predicate(),
            SymbolMatch::Id("b' OR 1=1").predicate()
        );
    }

    /// Re-review finding 5. The empty-id rule lived at the CALL SITES: copied
    /// into three of them and missed on the fourth
    /// (`search::context::find_community`), which forwarded `""` into a
    /// `get_context` answer. Owning it here is what makes the fourth consumer —
    /// and any fifth — correct without being told.
    #[test]
    fn an_empty_community_id_is_reported_as_no_community() {
        use super::super::{GraphStore, NODE_COMMUNITY, NODE_FUNCTION};

        let dir = tempfile::Builder::new()
            .prefix("membership_empty_community_id")
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");

        let qn = "m.rs::f";
        store
            .insert_node(
                NODE_COMMUNITY,
                &[
                    ("id", "''"),
                    ("name", "''"),
                    ("algorithm", "'louvain+c2'"),
                    ("resolution_param", "1.0"),
                    ("member_count", "1"),
                    ("modularity_contribution", "0.0"),
                ],
            )
            .expect("insert community");
        store
            .insert_node(
                NODE_FUNCTION,
                &[
                    ("id", "'m.rs::f'"),
                    ("name", "'f'"),
                    ("qualified_name", "'m.rs::f'"),
                    ("start_line", "1"),
                    ("end_line", "1"),
                    ("visibility", "'pub'"),
                    ("is_async", "false"),
                ],
            )
            .expect("insert fn");
        store
            .insert_edge("MemberOf_Function_Community", qn, "", &[])
            .expect("insert MemberOf");

        assert!(
            community_of(&store, "Function", SymbolMatch::Id(qn)).is_none(),
            "an empty Community.id identifies no community, for EVERY consumer"
        );
        assert!(
            community_of(&store, "Function", SymbolMatch::QualifiedName(qn)).is_none(),
            "and on every match arm"
        );
    }
}
