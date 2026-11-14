//! `detect_changes` with `base_graph_path` (issue #319): the base snapshot's
//! side of a comparison between two refs, each read against the graph
//! indexed at that ref (`git_diff::compare_snapshots`).

use crate::git_diff::{self, DiffAnalysis};
use crate::graph_cache;
use crate::graph_store::GraphStore;
use serde_json::{json, Map, Value};
use std::path::Path;

/// Prefix of every base-side reason in the merged `epistemic_reasons`: the
/// base side's reasons speak of the base graph, not of `graph_path`.
pub(super) const BASE_REASON_PREFIX: &str = "base snapshot: ";

/// The `base_snapshot` section of the response. Its symbol list is bounded
/// like the head's; its reasons join the response's `epistemic_reasons`, and
/// a lower-bound base side (or a cut list) makes the whole answer one.
pub(super) fn section(
    base: &DiffAnalysis,
    base_graph_path: &str,
    epistemic: &mut &'static str,
    reasons: &mut Vec<String>,
) -> Value {
    let symbols = crate::response_budget::bound_values(
        base.symbols_affected
            .iter()
            .filter_map(|s| serde_json::to_value(s).ok())
            .collect(),
        crate::response_budget::per_section_chars(),
    );
    let lower = crate::epistemic::Boundary::LowerBound.as_str();
    if base.epistemic == lower {
        *epistemic = lower;
    }
    reasons.extend(
        base.epistemic_reasons
            .iter()
            .map(|r| format!("{BASE_REASON_PREFIX}{r}")),
    );
    if symbols.truncated {
        *epistemic = lower;
        reasons.push(format!(
            "{BASE_REASON_PREFIX}symbols_affected lists {} of {} changed symbols \
             (response budget); narrow the diff to see the rest",
            symbols.items.len(),
            symbols.total_count
        ));
    }
    json!({
        "graph_path": base_graph_path,
        "symbols_affected": symbols.items,
        "symbols_affected_count": symbols.total_count,
        "symbols_affected_truncated": symbols.truncated,
        "epistemic": base.epistemic,
    })
}

/// `detect_changes` with `base_graph_path` (issue #319): the refs' diff read
/// against the head graph and against the base graph. It compares two refs
/// of one repository, so it takes `codebase_path`, never `diff_text`.
pub(super) fn compare_with_base(
    store: &GraphStore,
    base_graph: &str,
    args: &Map<String, Value>,
    (base_ref, head_ref): (&str, &str),
) -> Result<git_diff::SnapshotComparison, String> {
    if args.get("diff_text").is_some() {
        return Err(
            "base_graph_path compares two refs: give codebase_path with base_ref/head_ref, \
             not diff_text"
                .to_string(),
        );
    }
    let repo = args
        .get("codebase_path")
        .and_then(|v| v.as_str())
        .ok_or("base_graph_path requires codebase_path")?;
    let repo_path = Path::new(repo);
    if !repo_path.exists() {
        return Err(format!("codebase_path does not exist: {repo}"));
    }
    let base_path = Path::new(base_graph);
    if !base_path.exists() {
        return Err(format!("base_graph_path does not exist: {base_graph}"));
    }
    let base_store = graph_cache::open_cached(base_path)?;
    let refs = git_diff::RefRange {
        codebase_path: repo_path,
        base_ref,
        head_ref,
    };
    git_diff::compare_snapshots(store, &base_store, &refs)
}
