// process_impact_handlers::coverage_evidence — `exact` must be earned.
//
// source: ADR-9839, measured on DYResearch/dy-wcet @ 1e93ccd on 2026-09-09.

use std::path::Path;

use serde_json::{json, Value};

use crate::epistemic;
use crate::indexing_handlers;

/// The four gap buckets `indexing_handlers::coverage_summary` reports. Any
/// non-zero count means the index knows it did not cover something.
const GAP_BUCKETS: [&str; 4] = [
    "parse_incomplete",
    "skipped",
    "quarantined",
    "outside_build_targets",
];

/// Downgrades an `exact` blast radius to `lower-bound` unless the graph carries
/// a coverage record showing no known gap.
///
/// precondition: `out` is a `get_impact` response carrying `epistemic` and an
/// `epistemic_reasons` array; `graph_path` is the graph directory.
/// postcondition: `epistemic` is `exact` only when a coverage sidecar exists
/// AND reports zero gaps; every downgrade appends exactly one reason naming
/// why. A response that is already `lower-bound` is untouched.
pub(crate) fn require_coverage_evidence(out: &mut Value, graph_path: &Path) {
    if out.get("epistemic").and_then(Value::as_str) != Some(epistemic::Boundary::Exact.as_str()) {
        return;
    }
    let summary = indexing_handlers::coverage_summary_for_graph(graph_path);
    let Some(reason) = missing_evidence_reason(&summary) else {
        return;
    };
    out["epistemic"] = json!(epistemic::Boundary::LowerBound.as_str());
    if let Some(reasons) = out["epistemic_reasons"].as_array_mut() {
        reasons.push(json!(reason));
    }
}

/// `None` when the summary is positive evidence of full coverage. `Some(reason)`
/// when it is absent or reports a gap, with the reason naming which.
fn missing_evidence_reason(summary: &Value) -> Option<String> {
    if summary.is_null() {
        return Some(
            "this graph carries no coverage record, so completeness cannot be \
             established; `exact` would rest on the absence of a signal rather \
             than on a signal. Re-index to generate one."
                .to_string(),
        );
    }
    let flagged: Vec<String> = GAP_BUCKETS
        .iter()
        .filter_map(|bucket| named_count(summary, bucket))
        .collect();
    if flagged.is_empty() {
        return None;
    }
    Some(format!(
        "the index reports files it did not fully cover ({}); a caller in one of \
         them cannot appear in this graph, so the blast radius is a lower bound. \
         Enumerate them with query_graph(graph=\"missed\").",
        flagged.join(", ")
    ))
}

/// `Some("<bucket> N")` when that bucket's count is non-zero.
fn named_count(summary: &Value, bucket: &str) -> Option<String> {
    let count = summary.get(bucket)?.get("count")?.as_u64()?;
    if count == 0 {
        return None;
    }
    Some(format!("{bucket} {count}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact_response() -> Value {
        json!({
            "epistemic": epistemic::Boundary::Exact.as_str(),
            "epistemic_reasons": [],
        })
    }

    fn boundary_of(out: &Value) -> &str {
        out["epistemic"].as_str().expect("epistemic")
    }

    /// The defect this module exists for: a graph with no coverage record
    /// cannot support an `exact` claim, because nothing positive attests that
    /// the files holding potential callers were read at all.
    #[test]
    fn no_coverage_record_cannot_support_exact() {
        let reason = missing_evidence_reason(&Value::Null).expect("a reason");
        assert!(
            reason.contains("no coverage record"),
            "reason must name the absent record, got {reason}"
        );
    }

    /// A clean coverage record IS positive evidence, so `exact` survives it.
    /// Without this the rule would collapse to "never exact", which is honest
    /// but carries no information.
    #[test]
    fn a_clean_coverage_record_leaves_exact_standing() {
        let summary = json!({
            "parse_incomplete": { "count": 0 },
            "skipped": { "count": 0 },
            "quarantined": { "count": 0 },
            "outside_build_targets": { "count": 0 },
        });
        assert!(missing_evidence_reason(&summary).is_none());
    }

    /// Every bucket must block on its own. A test that only covered one would
    /// pass while the other three silently kept fabricating `exact`.
    #[test]
    fn each_gap_bucket_blocks_exact_on_its_own() {
        for bucket in GAP_BUCKETS {
            let mut summary = json!({
                "parse_incomplete": { "count": 0 },
                "skipped": { "count": 0 },
                "quarantined": { "count": 0 },
                "outside_build_targets": { "count": 0 },
            });
            summary[bucket]["count"] = json!(3);
            let reason = missing_evidence_reason(&summary)
                .unwrap_or_else(|| panic!("{bucket} must block exact"));
            assert!(
                reason.contains(bucket),
                "the reason must name {bucket}, got {reason}"
            );
        }
    }

    /// A response that is already a lower bound is not touched, and no second
    /// reason is appended.
    #[test]
    fn an_existing_lower_bound_is_left_alone() {
        let mut out = json!({
            "epistemic": epistemic::Boundary::LowerBound.as_str(),
            "epistemic_reasons": ["an earlier reason"],
        });
        require_coverage_evidence(&mut out, Path::new("/nonexistent/graph"));
        assert_eq!(boundary_of(&out), epistemic::Boundary::LowerBound.as_str());
        assert_eq!(
            out["epistemic_reasons"].as_array().expect("reasons").len(),
            1,
            "no second reason on an already-hedged response"
        );
    }

    /// End to end on a path with no sidecar: `exact` is downgraded and exactly
    /// one reason is appended.
    #[test]
    fn exact_is_downgraded_when_no_sidecar_exists() {
        let mut out = exact_response();
        require_coverage_evidence(&mut out, Path::new("/nonexistent/graph"));
        assert_eq!(boundary_of(&out), epistemic::Boundary::LowerBound.as_str());
        assert_eq!(
            out["epistemic_reasons"].as_array().expect("reasons").len(),
            1
        );
    }
}
