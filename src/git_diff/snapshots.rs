// git_diff::snapshots — one change between two snapshots of a repository,
// read line by line against one graph per snapshot (issue #319).
//
// `analyze_git_diff` reads `git diff base..head` against a single graph, the
// head's: a line the change removes is placed at its new-side position, and
// a file the change deletes has no File in that graph. Here each side is read
// against the graph indexed at its own ref. The reversed diff
// (`head..base`) names the same files with the sides swapped, so its
// new-side lines are the base snapshot's and a deleted file is new there.

use super::{
    build_analysis, git_diff_text, merge_by_path, parse_unified_diff, DiffAnalysis, FileHunk,
};
use crate::graph_store::{cypher_str, GraphStore};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

/// Both sides of one change between two snapshots of a repository, each
/// attributed to the graph indexed at its own ref (issue #319).
pub struct SnapshotComparison {
    /// The diff against the head graph: every file the diff names
    /// (`files_changed`), the head symbols its added and modified files
    /// change, the risk and the dependency confidence. A file the change
    /// deletes is the base side's.
    pub head: DiffAnalysis,
    /// The base graph's symbols whose lines the change removes or rewrites:
    /// `deleted` for a file the change deletes, `modified` for a file both
    /// snapshots hold. A file the change adds is the head side's. Its
    /// `epistemic_reasons` speak of the base graph.
    pub base: DiffAnalysis,
}

/// Two refs of the repository at `codebase_path`.
pub struct RefRange<'a> {
    pub codebase_path: &'a Path,
    pub base_ref: &'a str,
    pub head_ref: &'a str,
}

/// Compares two snapshots of the repository at `codebase_path`, line by line:
/// `git diff base_ref..head_ref` read against `head_store` (indexed at
/// `head_ref`) and, reversed (`head_ref..base_ref`), against `base_store`
/// (indexed at `base_ref`), whose new-side lines are the base snapshot's.
/// The reversed diff names the same files with the sides swapped, so a file
/// the change deletes is new there and its symbols are read whole from the
/// base graph, where the head graph has no File for it.
pub fn compare_snapshots(
    head_store: &GraphStore,
    base_store: &GraphStore,
    refs: &RefRange<'_>,
) -> Result<SnapshotComparison, String> {
    let forward = git_diff_text(refs.codebase_path, refs.base_ref, refs.head_ref)?;
    let reverse = git_diff_text(refs.codebase_path, refs.head_ref, refs.base_ref)?;
    let mut head = analyze_present_side(head_store, &forward)?;
    let mut base = analyze_present_side(base_store, &reverse)?;
    base.files_changed = head.files_changed;
    for symbol in &mut base.symbols_affected {
        // The reversed diff adds what the change deletes.
        if symbol.change_type == "added" {
            symbol.change_type = "deleted".to_string();
        }
    }
    mark_one_sided(&mut head, base_store, "added", "the base");
    mark_one_sided(&mut base, head_store, "deleted", "the head");
    Ok(SnapshotComparison { head, base })
}

/// A `modified` symbol of one side that the other graph does not hold (same
/// label, same qualified name) was added (head side) or deleted (base side)
/// inside a file both snapshots hold. When the lookup fails the symbol stays
/// `modified`, which is still true of its lines, and the side is a lower
/// bound.
fn mark_one_sided(side: &mut DiffAnalysis, other: &GraphStore, one_sided: &str, other_name: &str) {
    let mut by_label: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in side
        .symbols_affected
        .iter()
        .filter(|s| s.change_type == "modified")
    {
        by_label
            .entry(s.label.clone())
            .or_default()
            .insert(s.qualified_name.clone());
    }
    let mut held: BTreeSet<(String, String)> = BTreeSet::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    for (label, names) in &by_label {
        match names_held(other, label, names) {
            Ok(found) => held.extend(found.into_iter().map(|qn| (label.clone(), qn))),
            Err(_) => {
                failed.insert(label.clone());
            }
        }
    }
    let mut unchecked = 0_usize;
    for s in side
        .symbols_affected
        .iter_mut()
        .filter(|s| s.change_type == "modified")
    {
        if failed.contains(&s.label) {
            unchecked += 1;
        } else if !held.contains(&(s.label.clone(), s.qualified_name.clone())) {
            s.change_type = one_sided.to_string();
        }
    }
    if unchecked > 0 {
        side.epistemic = crate::epistemic::Boundary::LowerBound.as_str();
        side.epistemic_reasons.push(format!(
            "{unchecked} modified symbol(s) could not be looked up in {other_name} \
             graph (query failed); whether they exist on both sides is unknown"
        ));
    }
}

/// The names of `names` that `store` holds as a `label` node.
fn names_held(
    store: &GraphStore,
    label: &str,
    names: &BTreeSet<String>,
) -> Result<Vec<String>, String> {
    // A label is an identifier of the schema, never caller text; one that is
    // not cannot be spliced into the statement.
    if label.is_empty() || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("not a node label: {label:?}"));
    }
    let list: Vec<String> = names.iter().map(|n| cypher_str(n)).collect();
    let cypher = format!(
        "MATCH (n:{label}) WHERE n.qualified_name IN [{}] RETURN DISTINCT n.qualified_name",
        list.join(", ")
    );
    let qr = store.execute_query(&cypher)?;
    Ok(qr
        .rows
        .into_iter()
        .filter_map(|r| r.into_iter().next())
        .collect())
}

/// `diff_text` against the graph of its new side, without the sections that
/// delete a file: those exist only on the other side, whose graph attributes
/// them. Sections are dropped before they are merged by path, so a type
/// change (a file replaced by a symlink, or the reverse: one deleting and one
/// adding section for the same path) keeps its adding section here and its
/// deleting one on the other side (Codex review of PR #540). `files_changed`
/// still counts every path of the diff.
fn analyze_present_side(store: &GraphStore, diff_text: &str) -> Result<DiffAnalysis, String> {
    let sections = parse_unified_diff(diff_text)?;
    let files_changed = sections
        .iter()
        .map(|h| h.file_path.as_str())
        .collect::<HashSet<_>>()
        .len() as u64;
    let mut present: Vec<FileHunk> = sections.into_iter().filter(|h| !h.is_deleted).collect();
    // A pure removal on this side is a run of added lines on the other side,
    // which attributes it to its own symbols or reports it as its own gap. It
    // still marks the symbol of this side that holds it, but is no gap here:
    // a function added between two others would otherwise make the base side
    // a lower bound (PR #540 review, note 1).
    for hunk in &mut present {
        hunk.nonblank_gaps.clear();
    }
    let mut analysis = build_analysis(store, &merge_by_path(present))?;
    analysis.files_changed = files_changed;
    Ok(analysis)
}
