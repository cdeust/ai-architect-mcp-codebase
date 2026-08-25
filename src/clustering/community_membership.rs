// clustering::community_membership — the (symbol -> community) query API
// consumed by the `cluster_graph` MCP tool's response and by `query_graph`.
//
// Extracted from community.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// clustering vocabulary exactly as when this lived in one module.

use super::*;

/// One (symbol → community) membership row, derived from the persisted
/// MemberOf_<Label>_Community edge tables.
pub struct ClusterMembership {
    pub qualified_name: String,
    pub community_id: String,
    pub cluster_id: i64,
}

/// Bounded view over the full membership mapping. `total` is the pre-cap
/// count; when `truncated_at` is `Some(n)`, `entries.len() == n < total`
/// and the remainder is still reachable via `query_graph`.
pub struct ClusterMemberships {
    pub entries: Vec<ClusterMembership>,
    pub truncated_at: Option<usize>,
    pub total: usize,
}

pub(super) const CLUSTERS_RESPONSE_CAP: usize = 10_000;

/// Node labels that carry a `MemberOf_<Label>_Community` table. Shared with
/// `community_persist`, which only emits membership edges for these labels
/// (File nodes take part in the Louvain adjacency as containment carriers
/// but have no membership table — see that module's doc comment).
pub(super) const MEMBEROF_LABELS: &[&str] = &[
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Trait",
    "Constant",
    "TypeAlias",
    "Module",
];

/// Collect per-symbol community memberships by scanning every
/// `MemberOf_<Label>_Community` edge table. The response is capped at
/// `CLUSTERS_RESPONSE_CAP` entries; the full mapping remains queryable via
/// `query_graph` against the same edge tables.
pub fn collect_cluster_memberships(store: &GraphStore) -> Result<ClusterMemberships, String> {
    let mut entries: Vec<ClusterMembership> = Vec::new();
    for label in MEMBEROF_LABELS {
        let rel = format!("MemberOf_{label}_Community");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(c:Community) \
             RETURN n.qualified_name, c.id"
        );
        let qr = match store.execute_query(&cypher) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in qr.rows {
            if row.len() < 2 {
                continue;
            }
            // An empty `Community.id` identifies no community. This scan is a
            // separate caller from the `membership` traversals — it sweeps
            // whole edge tables rather than starting from one symbol — so it
            // restates the rule instead of routing through them. Forwarding an
            // empty id here made `cluster_graph`/`query_graph` disagree with
            // `get_impact` and `get_context` about the same symbol.
            if row[1].is_empty() {
                continue;
            }
            let cid = cluster_id_from_community_id(&row[1]);
            entries.push(ClusterMembership {
                qualified_name: row[0].clone(),
                community_id: row[1].clone(),
                cluster_id: cid,
            });
        }
    }
    Ok(sort_and_cap_memberships(entries))
}

/// Sort entries deterministically before applying the 10k truncation cap.
/// Must-fix from d-review.md §6: lbug/Kuzu row order per query is not
/// guaranteed, so an unsorted truncation would drop arbitrary entries per
/// run and break Q12 ARI reproducibility on graphs exceeding the cap.
fn sort_and_cap_memberships(mut entries: Vec<ClusterMembership>) -> ClusterMemberships {
    entries.sort_by(|a, b| {
        a.qualified_name
            .cmp(&b.qualified_name)
            .then_with(|| a.community_id.cmp(&b.community_id))
    });
    let total = entries.len();
    let truncated_at = if total > CLUSTERS_RESPONSE_CAP {
        entries.truncate(CLUSTERS_RESPONSE_CAP);
        Some(CLUSTERS_RESPONSE_CAP)
    } else {
        None
    };
    ClusterMemberships {
        entries,
        truncated_at,
        total,
    }
}

/// community_id persisted by `persist_communities` is
/// `community::louvain::<gamma>::<N>`. Extract the trailing integer so
/// the bench harness (which scores clusters via adjusted Rand index on
/// integer labels) can map community ids without parsing the prefix.
pub fn cluster_id_from_community_id(community_id: &str) -> i64 {
    community_id
        .rsplit("::")
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_store::{NODE_COMMUNITY, NODE_FUNCTION};

    /// Inserts a Community and one Function that is a member of it. `cid` may
    /// be empty — that degenerate shape is the subject of the test below.
    fn insert_member_of(store: &GraphStore, cid: &str, member: &str) {
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
        let esc = cypher_str(member);
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
            .expect("fn");
        store
            .insert_edge("MemberOf_Function_Community", member, cid, &[])
            .expect("MemberOf");
    }

    /// Round-5 finding 4. `collect_cluster_memberships` sweeps whole edge
    /// tables rather than starting from a symbol, so no round's centralization
    /// reached it — and it feeds `cluster_graph`/`query_graph`. Forwarding an
    /// empty `Community.id` here made those tools disagree with `get_impact`
    /// and `get_context` about the same symbol, which is the exact divergence
    /// this fix line has been closing one caller at a time.
    #[test]
    fn a_membership_with_an_empty_community_id_is_not_reported() {
        let dir = tempfile::Builder::new()
            .prefix("cluster_memberships_empty_id")
            .tempdir()
            .expect("tempdir");
        let store = GraphStore::open_or_create(&dir.path().join("db")).expect("open");
        store.create_schema().expect("schema");

        insert_member_of(&store, "", "m.rs::degenerate");
        insert_member_of(&store, "community::real", "m.rs::real");

        // Precondition: BOTH edges exist, so the filter is what removes one.
        assert_eq!(
            store
                .execute_query(
                    "MATCH (n:Function)-[:MemberOf_Function_Community]->(c:Community) \
                     RETURN c.id"
                )
                .expect("probe")
                .rows
                .len(),
            2
        );

        let reported: Vec<String> = collect_cluster_memberships(&store)
            .expect("collect")
            .entries
            .iter()
            .map(|m| m.qualified_name.clone())
            .collect();
        assert_eq!(
            reported,
            vec!["m.rs::real".to_string()],
            "a membership whose community id is empty names no community and \
             must not be reported"
        );
    }
}
