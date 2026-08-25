// graph_store::membership — the two clustering-membership traversals the read
// side shares.
//
// `MemberOf_<Label>_Community` and `ParticipatesIn_<Label>_Process` were each
// spelled out at both ends of the pipeline, and in three further places
// besides: `search` enriching every ranked hit, `search::context` answering
// get_context, `clustering::impact` collecting a blast radius,
// `security_gates` classifying an auth community, and `prd_validator`'s two
// axes. Five copies of one traversal is how a fix to one is missed on the
// others — fleet-watch#16 was exactly that, on the search copy, when the
// defence was still escaping rather than binding.
//
// They live here, beside the schema that declares the tables, because every
// caller already depends on `graph_store` and none depends on the others.
//
// THE VALUE NEVER ENTERS THE QUERY TEXT. Every `SymbolMatch` arm carries a RAW
// value, `predicate()` returns a CONSTANT `&'static str` referencing the bound
// parameter `$v`, and `params()` hands the value to the engine beside the
// statement. Nothing in this module escapes anything, because there is nothing
// here to escape.
//
// Two earlier revisions each stopped one step short. The first took an
// already-escaped literal on one arm, guarded only by a doc comment — a raw
// `&str` that must already be quoted is not an API, it is a trap, and it handed
// the fleet-watch#16 injection class back to a call site where nothing checked
// it. The second escaped inside the module, which was correct but still built
// the value into the text. Binding removes the question: no escaping rule
// stands between an adversarially-named indexed file and the parser, and
// because the text no longer varies with the value, one cached plan serves
// every call instead of one plan per distinct qualified name.

use super::GraphStore;
use lbug::Value;

/// How a membership traversal identifies the symbol it starts from.
///
/// Every arm carries a RAW value. Nothing here escapes it: `predicate()` names
/// the bound parameter `$v` and `params()` hands the value to the engine, so it
/// never reaches the query text. See the module header.
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
/// # Row-selection invariant
///
/// Derived and written BEFORE the query, because the previous two attempts
/// iterated on the SQL and each shipped a different wrong answer.
///
/// The question is: of the `Community` rows reachable from this symbol under
/// this label, which ONE is the answer?
///
///   I1. ELIGIBILITY — a row whose `c.id` is empty identifies no community and
///       is therefore not a candidate at all. It is excluded BEFORE any row is
///       chosen, never after.
///   I2. PREFERENCE — a non-empty id always beats an empty one. I1 makes this
///       automatic: empties are not in the running.
///   I3. DETERMINISM — among eligible rows the choice must not vary between
///       two calls on the same graph, so a total order (`c.id`, which is
///       unique per Community) decides.
///   I4. HONEST ABSENCE — when no eligible row exists, the answer is `None`.
///
/// Why I1 must run before the LIMIT, which is exactly what was wrong. Ordering
/// by `c.id` ascending puts `""` FIRST (`"" < "community::…"`), so `LIMIT 1`
/// selected the degenerate row whenever one existed, and the emptiness check
/// after it then turned that into `None`. A symbol with a real community AND a
/// stale degenerate edge reported "no community" — deterministically, and on
/// every consumer at once: `find_auth_communities` would read an
/// auth-community member as ungoverned. The round-4 intent (make the
/// degenerate case deterministic) was right; the direction was backwards.
///
/// `c.id <> ''` in the WHERE implements I1, so `ORDER BY` is left to do only
/// I3. The predicate is PARENTHESISED because `SymbolMatch` may expand to
/// `n.id = $v OR n.qualified_name = $v`, and `A OR B AND C` parses as
/// `A OR (B AND C)` — without the parentheses the eligibility filter would
/// silently apply to one arm only.
///
/// A well-formed graph never exercises any of this: `community_persist` writes
/// one `MemberOf` edge per node per run with a non-empty prefixed id, and
/// `cluster_graph` purges before writing. The invariant earns its place against
/// a graph this code did not write — a partial restore, a hand-edited store.
///
/// `LIMIT 1` is load-bearing rather than cosmetic: this runs once per candidate
/// on the substring fallback, which scans every node of every searchable label,
/// so an uncapped probe would materialize each node's whole `MemberOf` row set
/// on the hottest read path. [`community_ids`] is the call for wanting all of
/// them.
pub fn community_of(
    store: &GraphStore,
    label: &str,
    symbol: SymbolMatch<'_>,
) -> Option<CommunityRow> {
    let rel = format!("MemberOf_{label}_Community");
    let cypher = format!(
        "MATCH (n:{label})-[:{rel}]->(c:Community) WHERE ({}) AND c.id <> '' \
         RETURN c.id, c.name, c.member_count ORDER BY c.id LIMIT 1",
        symbol.predicate()
    );
    let qr = store.query_prepared_params(&cypher, symbol.params()).ok()?;
    let row = qr.rows.first()?;
    if row.len() < 3 {
        return None;
    }
    // No emptiness check here: I1 excluded empties from the candidate set, so a
    // row that reaches this point is eligible by construction. Re-checking
    // after the LIMIT is what produced the bug — it could only convert a bad
    // selection into `None`, never select correctly.
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
///
/// EMPTY VALUES ARE DROPPED, for the same reason `community_of` drops an empty
/// `Community.id`: an identifier that is empty identifies nothing. Putting the
/// rule here rather than at a call site is deliberate — the previous round put
/// it in `community_of` alone, which left `community_ids` (feeding
/// `get_impact`) and `process_names` (four callers) still forwarding `""`, so
/// `get_impact` and `get_context` could disagree about the same symbol. This is
/// the ONE drain both list-returning traversals share; the call-site inventory
/// below is exhaustive as of 2026-08-25:
///
///   community_ids  -> clustering::impact::collect_communities
///   process_names  -> clustering::impact::collect_processes,
///                     prd_validator::axis_process::processes_of,
///                     search::enrichment::lookup_processes,
///                     search::context::find_processes
fn first_column(store: &GraphStore, cypher: &str, symbol: SymbolMatch<'_>) -> Vec<String> {
    match store.query_prepared_params(cypher, symbol.params()) {
        Ok(qr) => qr
            .rows
            .iter()
            .filter_map(|row| row.first())
            .filter(|value| !value.is_empty())
            .cloned()
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{cypher_str, GraphStore, NODE_COMMUNITY, NODE_FUNCTION, NODE_PROCESS};
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

    /// A fresh store with the schema created.
    fn fixture_store(prefix: &str) -> (tempfile::TempDir, GraphStore) {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");
        (dir, store)
    }

    /// One Function node keyed and named by `qn`.
    fn insert_function(store: &GraphStore, qn: &str) {
        let esc = cypher_str(qn);
        store
            .insert_node(
                NODE_FUNCTION,
                &[
                    ("id", &esc),
                    ("name", &esc),
                    ("qualified_name", &esc),
                    ("start_line", "1"),
                    ("end_line", "1"),
                    ("visibility", "'pub'"),
                    ("is_async", "false"),
                ],
            )
            .expect("insert fn");
    }

    /// Re-review finding 5. The empty-id rule lived at the CALL SITES: copied
    /// into three of them and missed on the fourth
    /// (`search::context::find_community`), which forwarded `""` into a
    /// `get_context` answer. Owning it here is what makes the fourth consumer —
    /// and any fifth — correct without being told.
    #[test]
    fn an_empty_community_id_is_reported_as_no_community() {
        let (_dir, store) = fixture_store("membership_empty_community_id");
        let qn = "m.rs::f";
        insert_function(&store, qn);
        add_community(&store, "", qn);

        assert!(
            community_of(&store, "Function", SymbolMatch::Id(qn)).is_none(),
            "an empty Community.id identifies no community, for EVERY consumer"
        );
        assert!(
            community_of(&store, "Function", SymbolMatch::QualifiedName(qn)).is_none(),
            "and on every match arm"
        );
    }

    /// Round-3 finding 6. The empty-value rule lived in `community_of`, which
    /// left the two LIST-returning traversals — `community_ids` (feeding
    /// `get_impact`) and `process_names` (four callers) — still forwarding
    /// `""`. `get_impact` and `get_context` could then disagree about the same
    /// symbol's community. Both drain through `first_column`, so the rule
    /// belongs there.
    #[test]
    fn the_list_traversals_drop_empty_values_too() {
        let dir = tempfile::Builder::new()
            .prefix("membership_list_empty_values")
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
            .insert_node(NODE_PROCESS, &[("id", "'p0'"), ("name", "''")])
            .expect("insert process");
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
            .expect("MemberOf");
        store
            .insert_edge("ParticipatesIn_Function_Process", qn, "p0", &[])
            .expect("ParticipatesIn");

        assert!(
            community_ids(&store, "Function", SymbolMatch::Id(qn)).is_empty(),
            "an empty Community.id is not a community for get_impact either"
        );
        assert!(
            process_names(&store, "Function", SymbolMatch::Id(qn)).is_empty(),
            "an empty Process.name names no process"
        );
    }

    /// Inserts a Community and points `qn`'s MemberOf edge at it.
    fn add_community(store: &GraphStore, cid: &str, qn: &str) {
        store
            .insert_node(
                NODE_COMMUNITY,
                &[
                    ("id", &cypher_str(cid)),
                    ("name", &cypher_str(cid)),
                    ("algorithm", "'louvain+c2'"),
                    ("resolution_param", "1.0"),
                    ("member_count", "1"),
                    ("modularity_contribution", "0.0"),
                ],
            )
            .expect("community");
        store
            .insert_edge("MemberOf_Function_Community", qn, cid, &[])
            .expect("MemberOf");
    }

    /// Round-6 finding 1, the case NO previous test covered: a symbol carrying
    /// BOTH a real community edge and a degenerate one.
    ///
    /// `ORDER BY c.id LIMIT 1` sorts `""` first, so the degenerate row was
    /// selected and the post-LIMIT emptiness check turned it into `None` — the
    /// symbol read as having no community at all, deterministically. Only the
    /// single-degenerate-row case was tested, which passes either way.
    #[test]
    fn a_real_community_wins_over_a_degenerate_one() {
        let (_dir, store) = fixture_store("membership_real_beats_degenerate");
        let qn = "m.rs::f";
        insert_function(&store, qn);
        add_community(&store, "", qn);
        add_community(&store, "community::real", qn);

        // Precondition: BOTH edges exist, so the selection is what is tested.
        assert_eq!(
            store
                .execute_query(
                    "MATCH (n:Function)-[:MemberOf_Function_Community]->(c:Community) \
                     RETURN c.id"
                )
                .expect("probe")
                .rows
                .len(),
            2,
            "fixture precondition: the symbol must carry both edges"
        );

        for symbol in [SymbolMatch::Id(qn), SymbolMatch::QualifiedName(qn)] {
            let got = community_of(&store, "Function", symbol);
            assert_eq!(
                got.map(|c| c.id),
                Some("community::real".to_string()),
                "a real community must win over a degenerate one, on every arm"
            );
        }
    }
}
