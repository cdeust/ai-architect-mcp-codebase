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
// This guards the AUDIT, not the values: it cannot tell a guarded read from an
// unguarded one. What it makes impossible is a new reader arriving unnoticed.

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

/// A file is test code when it is a test module or carries `#[cfg(test)]`
/// around the query. Fixtures build their own degenerate graphs on purpose.
fn is_test_file(rel: &str, body: &str) -> bool {
    rel.ends_with("_tests.rs") || rel.ends_with("/tests.rs") || body.contains("#[cfg(test)]\nmod")
}

/// True when the file contains a Cypher pattern matching one of these nodes in
/// EXECUTABLE code.
///
/// Comment lines are excluded, and that is not cosmetic: several modules quote
/// a query in prose to explain a downstream invariant without reading anything
/// themselves (`process_impact_handlers` documents `get_processes`' lack of an
/// ORDER BY while consuming a decoded `ProcessInfo`). Listing those as readers
/// would put entries in the inventory that describe documentation rather than
/// code, and an inventory that overstates its scope stops being checkable.
fn reads_community_or_process(body: &str) -> bool {
    body.lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//"))
        .any(|line| line.contains(":Community)") || line.contains(":Process)"))
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
        if !reads_community_or_process(&body) {
            continue;
        }
        if is_test_file(&rel, &body) {
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
