// community_process_id_audit — the family guard for "an empty identifier
// identifies nothing".
//
// THREE CONSECUTIVE REVIEW ROUNDS found the same defect in a different caller:
// round 3 in `search::context::find_community`, round 4 in
// `clustering::impact::collect_communities` and `process_names`' four call
// sites, round 5 in `search::context::entry_point_processes` and
// `clustering::community_membership::collect_cluster_memberships`. Each round
// centralized "the" fix and each round a sibling survived, because the sweep
// was done by reading rather than by grepping.
//
// A defence only a reviewer enforces will be defeated (CONTRIBUTING, testing
// policy §3). So the classification is mechanical: every production file whose
// Cypher reads a Community or Process node must appear below WITH a reason. A
// new one fails this test until someone classifies it, which is the only step
// that has actually been skipped three times.
//
// WHAT THIS GUARD CANNOT DO — stated so nobody mistakes a green run for proof
// that the family is closed:
//
//   * It detects an UNLISTED file. It does not detect a WRONG per-site filter.
//     Five independent implementations of "an empty value means none" exist
//     across the classified sites; this test does not check that they agree, or
//     that any one of them is correct. A site could drop its filter entirely
//     and stay green here.
//   * It reads TEXT, not behaviour. A reader built through a helper, a
//     `format!` assembled elsewhere, or a label interpolated from a variable
//     does not match `:Community)` / `:Process)` at all, so it stays invisible
//     to this scan.
//
// What it makes impossible is exactly one thing: a NEW reader arriving without
// anyone deciding what it does about empty values. That is the step that was
// skipped three rounds running, which is why it is the step that is mechanised.

use std::fs;
use std::path::{Path, PathBuf};

/// Every production site whose Cypher matches a `Community` or `Process` node,
/// with what it reads and why that is safe. Audited 2026-08-25.
const CLASSIFIED: &[(&str, &str)] = &[
    // --- the centralized traversals: they OWN the rule -----------------------
    (
        "graph_store/membership.rs",
        "owner: community_of drops an empty c.id, first_column drops empty \
         values for community_ids and process_names",
    ),
    // --- reads no identifier at all -----------------------------------------
    ("git_diff.rs", "count(c) only — reads no identifier"),
    (
        "prd_input/matching.rs",
        "c.id is an INPUT filter; returns member_count only",
    ),
    // --- raw reads, each guarded --------------------------------------------
    (
        "security_gates/gates.rs",
        "find_auth_communities skips an empty c.id; run_s5 reads count(p) only. \
         Sweeps whole edge tables, so membership's per-symbol API does not fit",
    ),
    (
        "clustering/community_membership.rs",
        "collect_cluster_memberships skips an empty c.id (round-5 fix)",
    ),
    (
        "clustering/process.rs",
        "get_processes skips an empty p.name (found by the round-5 sweep, not by \
         review)",
    ),
    (
        "search/context.rs",
        "entry_point_processes drops an empty p.name (round-5 fix); the \
         participant half routes through membership",
    ),
    (
        "search/enrichment.rs",
        "load_community_sizes skips an empty c.id key; load_process_counts reads \
         count(p) keyed by qualified_name",
    ),
];

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Line ranges of the `#[cfg(test)]` regions in `body`.
///
/// Scoped by BRACE DEPTH from the item the attribute gates, so the region ends
/// where that item ends. The previous version asked only whether the attribute
/// appeared ANYWHERE in the file, which exempted every production file carrying
/// this repo's normal inline test module — `membership.rs`,
/// `community_membership.rs`, `context.rs` and `enrichment.rs`, four of the five
/// non-owner entries, all touched by the PR that added this guard. The guard
/// commissioned to stop "a sibling survives because the sweep missed it"
/// reproduced that exact failure inside itself.
fn cfg_test_line_ranges(body: &str) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = body.lines().collect();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].trim_start().starts_with("#[cfg(test)]") {
            i += 1;
            continue;
        }
        let (mut depth, mut opened, start) = (0i32, false, i);
        let mut j = i;
        while j < lines.len() {
            for byte in lines[j].bytes() {
                match byte {
                    b'{' => {
                        depth += 1;
                        opened = true;
                    }
                    b'}' => depth -= 1,
                    _ => {}
                }
            }
            if opened && depth <= 0 {
                break;
            }
            j += 1;
        }
        // `#[cfg(test)] mod name;` has no braces: it gates ANOTHER file, so it
        // covers nothing in this one.
        ranges.push(if opened { (start, j) } else { (start, start) });
        i = j + 1;
    }
    ranges
}

/// True when the file matches a Community or Process node in EXECUTABLE,
/// NON-TEST code.
///
/// Comment lines are excluded, and that is not cosmetic: several modules quote
/// a query in prose to explain a downstream invariant without reading anything
/// (`process_impact_handlers` documents `get_processes`' lack of an ORDER BY
/// while consuming a decoded `ProcessInfo`). Listing those as readers would put
/// entries in the inventory that describe documentation rather than code.
fn reads_community_or_process(body: &str) -> bool {
    let gated = cfg_test_line_ranges(body);
    body.lines().enumerate().any(|(n, line)| {
        let code = line.trim_start();
        !code.starts_with("//")
            && !gated.iter().any(|(from, to)| n >= *from && n <= *to)
            && (code.contains(":Community)") || code.contains(":Process)"))
    })
}

#[test]
fn every_community_or_process_reader_is_classified() {
    let root = src_root();
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    assert!(!files.is_empty(), "guard found no sources under {root:?}");

    let mut unclassified = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(body) = fs::read_to_string(path) else {
            continue;
        };
        if rel.ends_with("_tests.rs") || rel.ends_with("/tests.rs") {
            continue;
        }
        if !reads_community_or_process(&body) {
            continue;
        }
        if !CLASSIFIED.iter().any(|(file, _)| *file == rel) {
            unclassified.push(rel);
        }
    }

    assert!(
        unclassified.is_empty(),
        "these files read a Community or Process node and are not in the audit \
         inventory. An identifier read from the graph identifies something only \
         if it is non-empty — three review rounds in a row shipped a caller that \
         forwarded `\"\"`. Add each file to CLASSIFIED with what it reads and why \
         that is safe, after checking it:\n  {}",
        unclassified.join("\n  ")
    );
}

/// The inventory must not rot in the other direction either: an entry naming a
/// file that no longer reads these nodes is a stale claim about the codebase.
#[test]
fn the_audit_inventory_has_no_stale_entries() {
    let root = src_root();
    let mut stale = Vec::new();
    for (rel, _) in CLASSIFIED {
        let path = root.join(rel);
        let reads = fs::read_to_string(&path)
            .map(|body| reads_community_or_process(&body))
            .unwrap_or(false);
        if !reads {
            stale.push(*rel);
        }
    }
    assert!(
        stale.is_empty(),
        "audit entries for files that no longer read a Community or Process \
         node — remove them so the inventory keeps describing the code:\n  {}",
        stale.join("\n  ")
    );
}
