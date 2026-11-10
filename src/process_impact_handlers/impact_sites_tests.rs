// Issue #445: how `get_impact` renders the open call sites it names and how
// it sets `callers_completeness`. The code under test is in `impact.rs`, which
// mounts this module; the tests first lived in `impact_followups.rs` and moved
// here when that file would have passed the §4.1 500-line cap.

use super::*;
use crate::clustering::ImpactResult;
use crate::epistemic;

/// Issue #445: `lower-bound` alone does not tell an empty answer from a
/// partial one; `callers_completeness` does.
#[test]
fn callers_completeness_tells_none_found_from_partial() {
    use super::callers_completeness;
    assert_eq!(callers_completeness(true, true), "exact");
    assert_eq!(callers_completeness(true, false), "exact");
    assert_eq!(callers_completeness(false, true), "none_found");
    assert_eq!(callers_completeness(false, false), "found_more_may_exist");
}

fn base_impact() -> ImpactResult {
    ImpactResult {
        communities: Vec::new(),
        processes: Vec::new(),
        callers: Vec::new(),
        importers: Vec::new(),
        users: Vec::new(),
        implementors: Vec::new(),
        references: Vec::new(),
        epistemic: epistemic::Boundary::Exact,
        epistemic_reasons: Vec::new(),
        unresolved_callsites_naming_target: 0,
        unresolved_callsites_outside_targets: 0,
        unresolved_callsites_by_reason: Default::default(),
        unresolved_callsites_excluded_other_owner: 0,
        unresolved_callsite_outside_target_files: Vec::new(),
        unresolved_callsites_cfg_twins: 0,
        unresolved_callsites: Vec::new(),
        cfg_twins: Vec::new(),
        code_context_basis: "absent",
    }
}
fn site(i: usize) -> clustering::UnresolvedSite {
    clustering::UnresolvedSite {
        id: format!("src/a.rs::main::call@{i}:1#0"),
        file: "src/a.rs".to_string(),
        line: i as i64,
        callee_name: "s.response_of".to_string(),
        reason: "not_recorded".to_string(),
    }
}

fn attach_sites(impact: &ImpactResult, args: Value) -> Value {
    let mut out = json!({});
    let args = args.as_object().cloned().expect("object");
    super::attach_unresolved_callsites(&mut out, impact, &args);
    out
}

/// Issue #445: full objects by default, `file:line` under `detail='ids'`,
/// and `truncated` when the count exceeds the listing.
#[test]
fn unresolved_sites_follow_detail_and_flag_a_short_listing() {
    let mut impact = base_impact();
    impact.unresolved_callsites = vec![site(3), site(9)];
    impact.unresolved_callsites_naming_target = 2;
    let full = attach_sites(&impact, json!({}));
    assert_eq!(full["unresolved_callsites"][1]["line"], json!(9));
    assert_eq!(full["unresolved_callsites_truncated"], json!(false));
    let ids = attach_sites(&impact, json!({"detail": "ids"}));
    assert_eq!(
        ids["unresolved_callsites"],
        json!(["src/a.rs:3", "src/a.rs:9"])
    );
    let rows = attach_sites(&impact, json!({"format": "tabular"}));
    assert_eq!(
        rows["unresolved_callsites_columns"],
        json!(["id", "file", "line", "callee_name", "reason"])
    );
    assert_eq!(
        rows["unresolved_callsites"][0],
        json!([
            "src/a.rs::main::call@3:1#0",
            "src/a.rs",
            3,
            "s.response_of",
            "not_recorded"
        ])
    );
    impact.unresolved_callsites_naming_target = 60;
    let short = attach_sites(&impact, json!({}));
    assert_eq!(short["unresolved_callsites_truncated"], json!(true));
}

/// Issue #445: the listing is byte-bounded to a quarter of one section's share.
/// Rows of an eighth of a section plus their JSON overhead leave room for one
/// (`bound_values` always keeps the first), and the cut shows as `truncated`
/// although the count cap of 50 was not reached. The share itself is pinned by
/// the first assertion; the row count only proves the cut happens.
#[test]
fn unresolved_sites_stay_within_their_byte_share() {
    assert_eq!(
        super::unresolved_sites_chars(),
        response_budget::per_section_chars() / 4
    );
    let mut impact = base_impact();
    let mut wide = site(1);
    wide.callee_name = "x".repeat(response_budget::per_section_chars() / 8);
    impact.unresolved_callsites = vec![wide.clone(), wide.clone(), wide];
    impact.unresolved_callsites_naming_target = 3;
    let out = attach_sites(&impact, json!({}));
    let listed = out["unresolved_callsites"].as_array().expect("array").len();
    assert_eq!(listed, 1, "the byte share must cut the listing to one row");
    assert_eq!(out["unresolved_callsites_truncated"], json!(true));
}

/// Issue #445: completeness is read from the response after the steps that
/// lower `epistemic`; foreign callers alone are callers found.
#[test]
fn callers_completeness_counts_foreign_callers_and_reads_the_final_boundary() {
    use super::attach_callers_completeness;
    let impact = base_impact(); // no local callers, clustering says exact
    let mut out = json!({"epistemic": "lower-bound", "foreign_callers_total": 3});
    attach_callers_completeness(&mut out, &impact);
    assert_eq!(out["callers_completeness"], json!("found_more_may_exist"));
    let mut out = json!({"epistemic": "lower-bound"});
    attach_callers_completeness(&mut out, &impact);
    assert_eq!(out["callers_completeness"], json!("none_found"));
    let mut out = json!({"epistemic": "exact"});
    attach_callers_completeness(&mut out, &impact);
    assert_eq!(out["callers_completeness"], json!("exact"));
}

/// Issue #445: the lower edge of the byte share at the call site. Two rows of a
/// ninth of a section each fit in a quarter-section share, so a smaller budget
/// passed to `bound_values` (which always keeps the first row) would cut one.
#[test]
fn unresolved_sites_list_every_row_that_fits_the_byte_share() {
    let mut impact = base_impact();
    let mut wide = site(1);
    wide.callee_name = "x".repeat(response_budget::per_section_chars() / 9);
    impact.unresolved_callsites = vec![wide.clone(), wide];
    impact.unresolved_callsites_naming_target = 2;
    let out = attach_sites(&impact, json!({}));
    assert_eq!(
        out["unresolved_callsites"].as_array().expect("array").len(),
        2
    );
    assert_eq!(out["unresolved_callsites_truncated"], json!(false));
}
