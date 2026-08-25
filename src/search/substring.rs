// search::substring — the graph-only fallback scorer.
//
// Split from `search/mod.rs` when that file crossed the §4.1 cap. `search_graph`
// takes this path when no BM25 or vector index was built beside the graph; it
// scans every searchable label and scores each candidate on substring overlap
// alone. It is the v1 ranking, kept so a graph without a search index still
// answers.

use super::enrichment::{
    decode_row, lookup_community, lookup_processes, return_clause, BoostWeights, RankBoosts,
};
use super::qualified_name::file_path_of;
use super::{sort_and_truncate, SearchOptions, SearchResult, SEARCHABLE_LABELS};
use crate::graph_store::GraphStore;

/// Boost weights for the fallback path, which scores overlap in 0..1.
const SUBSTRING_WEIGHTS: BoostWeights = BoostWeights {
    small_community: 0.1,
    per_process: 0.05,
};

pub(super) fn search_substring(
    store: &GraphStore,
    terms: &[&str],
    options: &SearchOptions,
) -> Result<Vec<SearchResult>, String> {
    let boosts = RankBoosts::load(store);
    let mut results: Vec<SearchResult> = Vec::new();

    for &label in SEARCHABLE_LABELS {
        if let Some(ref filter) = options.label_filter {
            if !filter.eq_ignore_ascii_case(label) {
                continue;
            }
        }
        for c in fetch_candidates(store, label)? {
            let score = score_candidate(&c, terms, &boosts);
            if score < options.min_score {
                continue;
            }
            results.push(SearchResult {
                qualified_name: c.qualified_name,
                name: c.name,
                label: label.to_string(),
                file_path: c.file_path,
                score,
                community_id: c.community_id,
                process_names: c.process_names,
                start_line: c.start_line,
                end_line: c.end_line,
            });
        }
    }

    sort_and_truncate(&mut results, options.limit);
    Ok(results)
}

struct Candidate {
    qualified_name: String,
    name: String,
    file_path: String,
    community_id: Option<String>,
    process_names: Vec<String>,
    start_line: Option<u64>,
    end_line: Option<u64>,
}

/// Every node of `label`, decoded and enriched. Unlike the hybrid path this
/// scans the whole label — the substring scorer has no retriever to narrow the
/// candidate set for it.
fn fetch_candidates(store: &GraphStore, label: &str) -> Result<Vec<Candidate>, String> {
    let (clause, has_lines) = return_clause(label);
    let qr = store.execute_query(&format!("MATCH (n:{label}) RETURN {clause}"))?;

    let mut candidates = Vec::new();
    for row in &qr.rows {
        let Some(node) = decode_row(row, has_lines) else {
            continue;
        };
        candidates.push(Candidate {
            file_path: file_path_of(&node.qualified_name).to_string(),
            community_id: lookup_community(store, label, &node.id),
            process_names: lookup_processes(store, label, &node.id),
            qualified_name: node.qualified_name,
            name: node.name,
            start_line: node.start_line,
            end_line: node.end_line,
        });
    }
    Ok(candidates)
}

fn score_candidate(c: &Candidate, terms: &[&str], boosts: &RankBoosts) -> f64 {
    let name_lower = c.name.to_lowercase();
    let qn_lower = c.qualified_name.to_lowercase();

    let mut best_term_score: f64 = 0.0;
    for &term in terms {
        let ts = term_score(term, &name_lower, &qn_lower);
        if ts > best_term_score {
            best_term_score = ts;
        }
    }

    if best_term_score == 0.0 {
        return 0.0;
    }

    let all_match = terms
        .iter()
        .all(|t| qn_lower.contains(t) || name_lower.contains(t));
    let multi_bonus = if all_match && terms.len() > 1 {
        0.1
    } else {
        0.0
    };

    let boost = boosts.boost_for(
        &c.qualified_name,
        c.community_id.as_deref(),
        &SUBSTRING_WEIGHTS,
    );

    (best_term_score + multi_bonus + boost).min(1.0)
}

fn term_score(term: &str, name_lower: &str, qn_lower: &str) -> f64 {
    if name_lower == term {
        return 1.0;
    }
    if !name_lower.is_empty() && name_lower.contains(term) {
        let ratio = term.len() as f64 / name_lower.len() as f64;
        return 0.7 + 0.3 * ratio;
    }
    if !qn_lower.is_empty() && qn_lower.contains(term) {
        let ratio = term.len() as f64 / qn_lower.len() as f64;
        return 0.5 * (1.0 + ratio);
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_term_score_exact() {
        assert_eq!(term_score("main", "main", "src/main.rs::main"), 1.0);
    }

    #[test]
    fn test_term_score_contains_name() {
        let s = term_score(
            "handle",
            "handle_tool_call",
            "src/main.rs::handle_tool_call",
        );
        assert!(s > 0.7 && s < 1.0, "expected 0.7..1.0, got {s}");
    }

    #[test]
    fn test_term_score_contains_qn_only() {
        let s = term_score(
            "main.rs",
            "handle_tool_call",
            "src/main.rs::handle_tool_call",
        );
        assert!(s > 0.5 && s < 1.0, "expected 0.5..1.0, got {s}");
    }

    #[test]
    fn test_term_score_no_match() {
        assert_eq!(term_score("zzzzz", "main", "src/main.rs::main"), 0.0);
    }
}
