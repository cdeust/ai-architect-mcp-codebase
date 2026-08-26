//! `ingest_traces` — folding observed runtime calls into the static graph.
//!
//! Split out of `indexing_handlers` (fleet-watch#112 review round 5): that file
//! crossed the §4.1 500-line cap once the round-4 write-order work landed, and
//! trace ingestion is the concern that shares least with it — it neither indexes
//! nor writes a sidecar, it annotates an existing graph with what actually ran.

use serde_json::{json, Value};
use std::path::Path;

use crate::graph_cache;
use crate::history_handlers::{annotate_static_call, callable_label, upsert_observed_call};
use crate::indexing_handlers::COVERAGE_LIST_CAP;

// ---------------------------------------------------------------------------
// Stage 3 — ingest_traces (issue #58 runtime enrichment)
// ---------------------------------------------------------------------------

pub(crate) fn run_ingest_traces(arguments: &Value) -> Value {
    match do_ingest_traces(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "ingest_traces_failed", "message": msg
        }),
    }
}

/// Folds runtime caller→callee observations into the graph (issue #58): annotate
/// a matching static Calls edge with `observed_count`, or create an
/// `OBSERVED_CALLS` edge where static resolution found none (the divergence
/// signal — runtime truth the static analysis missed).
///
/// Input: `{graph_path, traces: [{caller, callee, count}]}`. `caller`/`callee`
/// are qualified names (`file::symbol`); `count` defaults to 1. Postconditions:
/// each trace either annotated a static Calls edge (matched), created/updated an
/// OBSERVED_CALLS edge (unmatched_created), or was recorded as unresolved
/// (endpoint not a Function/Method node). The response reports the three counts
/// plus a capped list of the created divergences and the unresolved names.
/// Sums observations per (caller, callee) so repeated pairs add up instead of
/// creating duplicate edges.
fn aggregate_traces(
    traces: &[Value],
) -> Result<std::collections::BTreeMap<(String, String), i64>, String> {
    let mut agg: std::collections::BTreeMap<(String, String), i64> =
        std::collections::BTreeMap::new();
    for t in traces {
        let caller = t.get("caller").and_then(|v| v.as_str());
        let callee = t.get("callee").and_then(|v| v.as_str());
        let (caller, callee) = match (caller, callee) {
            (Some(a), Some(b)) => (a.to_string(), b.to_string()),
            _ => return Err("each trace needs 'caller' and 'callee' strings".to_string()),
        };
        let count = t.get("count").and_then(|v| v.as_i64()).unwrap_or(1).max(1);
        *agg.entry((caller, callee)).or_insert(0) += count;
    }
    Ok(agg)
}

/// Running totals while folding runtime observations into the graph.
#[derive(Default)]
struct TraceTally {
    matched: u64,
    unmatched_created: u64,
    unresolved: Vec<String>,
    divergences: Vec<Value>,
}

impl TraceTally {
    /// Folds one observed call in: annotate the static edge if there is one,
    /// otherwise record the divergence as an OBSERVED_CALLS edge. An endpoint
    /// that is not a callable node is unresolved, not an error.
    fn absorb(
        &mut self,
        store: &crate::graph_store::GraphStore,
        caller: &str,
        callee: &str,
        count: i64,
    ) -> Result<(), String> {
        let from_label = callable_label(store, caller);
        let to_label = callable_label(store, callee);
        let (Some(from_label), Some(to_label)) = (from_label, to_label) else {
            if callable_label(store, caller).is_none() {
                self.unresolved.push(caller.to_string());
            }
            if callable_label(store, callee).is_none() {
                self.unresolved.push(callee.to_string());
            }
            return Ok(());
        };
        let static_table = format!("Calls_{from_label}_{to_label}");
        if annotate_static_call(store, &static_table, caller, callee, count)? {
            self.matched += 1;
            return Ok(());
        }
        let observed_table = format!("OBSERVED_CALLS_{from_label}_{to_label}");
        upsert_observed_call(store, &observed_table, caller, callee, count)?;
        self.unmatched_created += 1;
        if self.divergences.len() < COVERAGE_LIST_CAP {
            self.divergences.push(json!({
                "caller": caller, "callee": callee, "observed_count": count
            }));
        }
        Ok(())
    }
}

pub(crate) fn do_ingest_traces(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let traces = args
        .get("traces")
        .and_then(|v| v.as_array())
        .ok_or("missing required field 'traces' (array of {caller, callee, count})")?;

    let store = graph_cache::open_cached(graph_path)?;
    let mut tally = TraceTally::default();
    for ((caller, callee), count) in aggregate_traces(traces)? {
        tally.absorb(&store, &caller, &callee, count)?;
    }
    let TraceTally {
        matched,
        unmatched_created,
        mut unresolved,
        divergences,
    } = tally;

    unresolved.sort();
    unresolved.dedup();
    let unresolved_total = unresolved.len();
    unresolved.truncate(COVERAGE_LIST_CAP);

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "ingest_traces",
        "matched": matched,
        "unmatched_created": unmatched_created,
        "unresolved_names": unresolved_total,
        "unresolved_names_sample": unresolved,
        "observed_divergences": divergences,
        "note": "matched = a static Calls edge annotated with observed_count. \
                 unmatched_created = OBSERVED_CALLS edges where runtime saw a call \
                 static analysis did not — the valuable divergence signal. \
                 unresolved_names = trace endpoints that are not Function/Method \
                 nodes (wrong qualified name, or an uncovered/unindexed symbol).",
    }))
}
