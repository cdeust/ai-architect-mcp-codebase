// search::grouping — the process index over a page of ranked hits.
//
// Split from `search/mod.rs` when that file crossed the §4.1 cap. Grouping is
// a pure function of an already-ranked, already-paged result list: it reads no
// graph and holds no state, which is why it lives apart from the ranking paths
// that produce the list.

use std::collections::HashMap;

/// Bucket label for hits that participate in no process. Functions/Methods that
/// belong to no `Process` — and all non-callable kinds (Struct/Enum/Trait/…),
/// for which `lookup_processes` returns empty by construction — land here.
pub(crate) const NO_PROCESS_GROUP: &str = "(no process)";

/// Record group membership for `qn` under `key`, keeping each group's member
/// list de-duplicated and recording first-seen key order. Extracted from
/// `group_hits_by_process` to hold that function's control nesting at ≤2 levels
/// (coding-standards §7.2).
fn push_group_member(
    groups: &mut HashMap<String, Vec<String>>,
    seen_order: &mut Vec<String>,
    key: &str,
    qn: &str,
) {
    let bucket = groups.entry(key.to_string()).or_default();
    if !bucket.iter().any(|existing| existing == qn) {
        bucket.push(qn.to_string());
    }
    if !seen_order.iter().any(|k| k == key) {
        seen_order.push(key.to_string());
    }
}

/// Group ranked search hits by the processes they participate in, producing a
/// lightweight secondary index over the (already-ranked, already-paged) result
/// list. Each returned tuple is `(group_name, qualified_names_in_rank_order)`.
///
/// Semantics:
/// - A hit listing N processes appears in all N corresponding groups — it truly
///   participates in each. A hit listing none appears once in the trailing
///   [`NO_PROCESS_GROUP`] bucket.
/// - Within a group, qualified names preserve the input rank order.
/// - Named groups are ordered by descending member count, ties broken by
///   ascending group name. The [`NO_PROCESS_GROUP`] bucket, when present, always
///   sorts last regardless of size. This total order is a deterministic function
///   of the input, matching the cursor-stability discipline the caller relies on
///   (see `do_search_codebase`).
///
/// The result is an index *into* the supplied hits: every qualified name it
/// contains is present in `hits`, so callers keep the flat result list as the
/// single source of truth with no payload duplication.
pub fn group_hits_by_process(hits: &[(String, Vec<String>)]) -> Vec<(String, Vec<String>)> {
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen_order: Vec<String> = Vec::new();

    for (qn, processes) in hits {
        if processes.is_empty() {
            push_group_member(&mut groups, &mut seen_order, NO_PROCESS_GROUP, qn);
        } else {
            for process in processes {
                push_group_member(&mut groups, &mut seen_order, process, qn);
            }
        }
    }

    seen_order.sort_by(
        |a, b| match (a == NO_PROCESS_GROUP, b == NO_PROCESS_GROUP) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => groups[b].len().cmp(&groups[a].len()).then_with(|| a.cmp(b)),
        },
    );

    seen_order
        .into_iter()
        .map(|key| {
            let members = groups.remove(&key).unwrap_or_default();
            (key, members)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(qn: &str, procs: &[&str]) -> (String, Vec<String>) {
        (
            qn.to_string(),
            procs.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn group_preserves_rank_order_within_group() {
        // Two hits in the same process keep their input (ranked) order.
        let hits = [hit("a::first", &["flow"]), hit("a::second", &["flow"])];
        let grouped = group_hits_by_process(&hits);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].0, "flow");
        assert_eq!(grouped[0].1, vec!["a::first", "a::second"]);
    }

    #[test]
    fn group_multi_process_hit_appears_in_each_group() {
        // A hit participating in two processes is indexed under both.
        let hits = [hit("a::shared", &["checkout", "auth"])];
        let grouped = group_hits_by_process(&hits);
        let names: Vec<&str> = grouped.iter().map(|(p, _)| p.as_str()).collect();
        assert!(names.contains(&"checkout") && names.contains(&"auth"));
        for (_, members) in &grouped {
            assert_eq!(members, &vec!["a::shared".to_string()]);
        }
    }

    #[test]
    fn group_no_process_bucket_sorts_last() {
        // The (no process) bucket trails even when it is the largest group.
        let hits = [
            hit("a::x", &[]),
            hit("a::y", &[]),
            hit("a::z", &[]),
            hit("a::charged", &["pay"]),
        ];
        let grouped = group_hits_by_process(&hits);
        assert_eq!(grouped.first().unwrap().0, "pay");
        assert_eq!(grouped.last().unwrap().0, NO_PROCESS_GROUP);
        assert_eq!(grouped.last().unwrap().1.len(), 3);
    }

    #[test]
    fn group_orders_by_size_desc_then_name_asc() {
        // "big" has 2 members, "alpha" and "beta" have 1 each → size desc puts
        // "big" first; the two singletons tie on size and break by name asc.
        let hits = [
            hit("m::1", &["big"]),
            hit("m::2", &["big"]),
            hit("m::3", &["beta"]),
            hit("m::4", &["alpha"]),
        ];
        let order: Vec<String> = group_hits_by_process(&hits)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(order, vec!["big", "alpha", "beta"]);
    }

    #[test]
    fn group_empty_input_yields_no_groups() {
        assert!(group_hits_by_process(&[]).is_empty());
    }

    #[test]
    fn group_index_only_references_supplied_hits() {
        // Invariant: every qualified_name in the index is present in the input.
        let hits = [
            hit("a::one", &["f"]),
            hit("a::two", &[]),
            hit("a::three", &["f", "g"]),
        ];
        let input_qns: std::collections::HashSet<&str> =
            hits.iter().map(|(qn, _)| qn.as_str()).collect();
        for (_, members) in group_hits_by_process(&hits) {
            for qn in members {
                assert!(input_qns.contains(qn.as_str()), "dangling qn: {qn}");
            }
        }
    }
}
