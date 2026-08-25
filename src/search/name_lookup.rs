// search::name_lookup — resolving a caller-supplied name to a stored key.
//
// Split from `search/mod.rs` when that file crossed the §4.1 cap. Both
// `get_context` and `get_symbol` accept the forgiving input surface defined
// here, and `resolve_impact_target` builds on it, so the three-layer lookup is
// its own concern rather than a detail of any one tool.

use super::SEARCHABLE_LABELS;
use crate::graph_store::{cypher_str, GraphStore};

/// Error returned by `get_context` when a symbol cannot be resolved.
/// Carries "did you mean" suggestions so the MCP caller can surface them
/// verbatim instead of choking on a bare "symbol not found" string.
///
/// source: C-correctness bug 2 — callers naturally pass `src/main.rs::X`
/// while the graph stores `main.rs::X`; the old API returned a flat Err
/// that hid the near-misses.
#[derive(Debug, Clone)]
pub struct SymbolNotFound {
    pub input: String,
    pub did_you_mean: Vec<String>,
}

/// Three-layer qualified-name lookup.
/// Returns the resolved (stored) qualified_name on success, or a
/// `SymbolNotFound` carrying suggestions when every layer misses.
///
/// Used by both `get_context` and `get_symbol` so both tools share the
/// same forgiving input surface.
pub fn resolve_qualified_name(store: &GraphStore, input: &str) -> Result<String, SymbolNotFound> {
    // Layer 1 — exact.
    if let Some(qn) = exact_match_qn(store, input) {
        return Ok(qn);
    }

    // Layer 2 — strip first path component if the input has one.
    // Parser strips `src/` when building qualified_names, so callers who
    // naturally pass `src/main.rs::foo` must find `main.rs::foo`.
    if let Some(stripped) = strip_leading_path_component(input) {
        if let Some(qn) = exact_match_qn(store, &stripped) {
            return Ok(qn);
        }
    }

    // Layer 3 — name-only fuzzy. Return top candidates as suggestions.
    let leaf = input.rsplit("::").next().unwrap_or(input);
    let suggestions = find_name_candidates(store, leaf, 5);
    Err(SymbolNotFound {
        input: input.to_string(),
        did_you_mean: suggestions,
    })
}

// pub(crate): reused by prd_validator's unverifiable-file classification,
// which must apply the same src/-stripping retry this layer-2 lookup does
// before concluding a claimed file is outside the indexed graph.
pub(crate) fn strip_leading_path_component(input: &str) -> Option<String> {
    // Only act if the path portion (before `::`) has a `/`.
    let (path_part, rest) = match input.find("::") {
        Some(i) => (&input[..i], &input[i..]),
        None => (input, ""),
    };
    let idx = path_part.find('/')?;
    Some(format!("{}{}", &path_part[idx + 1..], rest))
}

fn exact_match_qn(store: &GraphStore, input: &str) -> Option<String> {
    let escaped = cypher_str(input);
    for &label in SEARCHABLE_LABELS {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = {escaped} OR n.id = {escaped} \
             RETURN n.qualified_name LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if !row.is_empty() {
                    return Some(row[0].clone());
                }
            }
        }
    }
    None
}

fn find_name_candidates(store: &GraphStore, name: &str, limit: usize) -> Vec<String> {
    let escaped = cypher_str(name);
    let mut out = Vec::new();
    for &label in SEARCHABLE_LABELS {
        if out.len() >= limit {
            break;
        }
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.name = {escaped} \
             RETURN n.qualified_name LIMIT {limit}"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if out.len() >= limit {
                    break;
                }
                if !row.is_empty() && !out.contains(&row[0]) {
                    out.push(row[0].clone());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_leading_path_component() {
        // source: C-correctness bug 2 — callers pass `src/main.rs::foo`,
        // the graph stores `main.rs::foo`. Layer 2 of the three-layer lookup
        // drops the first path component and retries.
        assert_eq!(
            strip_leading_path_component("src/main.rs::foo"),
            Some("main.rs::foo".to_string())
        );
        assert_eq!(
            strip_leading_path_component("src/foo/bar.rs::baz"),
            Some("foo/bar.rs::baz".to_string())
        );
        // No slash → nothing to strip.
        assert_eq!(strip_leading_path_component("main.rs::foo"), None);
        // Slash in the `::`-suffix is ignored — only the path part is split.
        assert_eq!(
            strip_leading_path_component("src/main.rs::Foo::bar"),
            Some("main.rs::Foo::bar".to_string())
        );
    }
}
