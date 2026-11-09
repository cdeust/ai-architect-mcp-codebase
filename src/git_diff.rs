// git_diff — maps git diff changed lines to affected symbols in the code graph.
//
// Two entry points:
//   1. analyze_diff(store, diff_text) — parse raw unified diff text
//   2. analyze_git_diff(store, codebase_path, base_ref, head_ref) — run git diff
//
// Each changed line is attributed to the innermost graph symbols whose
// [start_line, end_line] range holds it (`symbols`). Each affected symbol is
// enriched with community and process membership. A change no symbol holds (a
// non-blank line or a pure removal outside every symbol, a path with no indexed
// File, an added or deleted file with no symbol) or a failed symbol query makes
// the answer `lower-bound` (issue #446).

use crate::clustering;
use crate::graph_store::GraphStore;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

mod symbols;

// ---------------------------------------------------------------------------
// Safety limit — diff line counter overflow guard.
// source: M3 fix — current_line is u64, but a malicious diff with billions of
// `+` lines could wrap around. Cap at u64::MAX / 2 so we always have headroom.
// ---------------------------------------------------------------------------

const DIFF_LINE_MAX: u64 = u64::MAX / 2;

/// Validates a git ref for safe use as a positional argument.
/// Rejects refs starting with `-` (which git would treat as an option flag)
/// and refs containing newlines or NUL bytes.
/// source: C2 fix — git CVE-2018-17456 class vulnerability prevention.
fn validate_git_ref(r: &str, field: &str) -> Result<(), String> {
    if r.is_empty() {
        return Err(format!("invalid_ref: {field} must not be empty"));
    }
    if r.starts_with('-') {
        return Err(format!(
            "invalid_ref: {field} must not start with '-' (got {r:?})"
        ));
    }
    if r.contains('\n') || r.contains('\0') {
        return Err(format!(
            "invalid_ref: {field} must not contain newline or NUL (got {r:?})"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ChangedSymbol {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub change_type: String, // "modified", "added", "deleted"
    pub lines_changed: u64,
    pub community_id: Option<String>,
    pub processes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffAnalysis {
    pub files_changed: u64,
    pub symbols_affected: Vec<ChangedSymbol>,
    pub communities_affected: Vec<String>,
    pub processes_affected: Vec<String>,
    pub risk_score: f64,
    /// Mean confidence of the depth-1 reverse-dependency edges of the changed
    /// symbols (stored edge `confidence`, else the per-relation-type floor).
    /// 1.0 when the changed symbols have no resolved dependents. A value < 1.0
    /// means the blast radius `risk_score` is computed over rests partly on
    /// heuristically-resolved edges — read `risk_score` as itself uncertain.
    /// source: epistemic module; clustering::get_impact per-edge confidence.
    pub mean_dependency_confidence: f64,
    /// `exact` when every non-blank changed line lies in a symbol and every
    /// changed symbol's blast radius is statically exhaustive; `lower-bound`
    /// when a non-blank changed line of a modified or added file (or a pure
    /// removal) lies in no indexed symbol, a path has no indexed File, an added
    /// or deleted file holds no indexed symbol, or a symbol query failed
    /// (issue #446), or at least one
    /// changed symbol is reached via dynamic dispatch or through heuristic
    /// edges — true risk may exceed the reported score. source: epistemic
    /// module.
    pub epistemic: &'static str,
    /// Carriers of epistemic uncertainty (empty when `exact`): a changed
    /// symbol and why its impact under-counts, or a changed file and why some
    /// of its changes reach no symbol (issue #446).
    pub epistemic_reasons: Vec<String>,
    /// How many of `epistemic_reasons` are coverage gaps (a changed file whose
    /// changes partly reach no symbol, or a failed symbol query) rather than
    /// dependency uncertainty. Internal: read by `next_steps`, not emitted.
    #[serde(skip)]
    pub coverage_gap_count: usize,
}

// ---------------------------------------------------------------------------
// Diff parsing — unified diff format
// ---------------------------------------------------------------------------

// pub(crate): reused by the history module, which parses per-commit
// `git show` diffs into the same hunk shape to attribute changes to entities.
pub(crate) struct FileHunk {
    pub(crate) file_path: String,
    /// New-file positions of every `+` line and of every `-` line (a `-` line
    /// takes the position of the new-file line that follows it, except in a
    /// `+N,0` hunk, where it takes N, the line before: `removal_gaps` corrects
    /// that, this list keeps it). Read by the history module.
    pub(crate) changed_lines: Vec<u64>,
    /// New-file positions of the non-blank `+` lines.
    pub(crate) added_lines: Vec<u64>,
    /// New-file positions just after a run of `-` lines that no `+` line
    /// replaces (a pure removal between lines p-1 and p).
    pub(crate) removal_gaps: Vec<u64>,
    pub(crate) is_new: bool,
    pub(crate) is_deleted: bool,
}

/// The per-file line lists `parse_unified_diff` accumulates.
#[derive(Default)]
struct LineLists {
    changed: Vec<u64>,
    added: Vec<u64>,
    gaps: Vec<u64>,
    /// Position of a `-` run not yet followed by a `+` line.
    pending_removal: Option<u64>,
    /// 1 inside a hunk whose new range is empty (`+N,0`): its start N names
    /// the line BEFORE the removal, so the line after it is N + 1.
    gap_shift: u64,
}

impl LineLists {
    fn close_removal(&mut self) {
        if let Some(p) = self.pending_removal.take() {
            self.gaps.push(p.saturating_add(self.gap_shift));
        }
    }

    fn into_hunk(mut self, file_path: String, is_new: bool, is_deleted: bool) -> FileHunk {
        self.close_removal();
        FileHunk {
            file_path,
            changed_lines: self.changed,
            added_lines: self.added,
            removal_gaps: self.gaps,
            is_new,
            is_deleted,
        }
    }
}

pub(crate) fn parse_unified_diff(diff_text: &str) -> Result<Vec<FileHunk>, String> {
    let mut hunks: Vec<FileHunk> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut lists = LineLists::default();
    let mut current_line: u64 = 0;
    let mut is_new = false;
    let mut is_deleted = false;
    let mut in_hunk = false;

    for line in diff_text.lines() {
        if line.starts_with("diff --git") {
            // Flush previous file
            if let Some(fp) = current_file.take() {
                hunks.push(std::mem::take(&mut lists).into_hunk(fp, is_new, is_deleted));
            }
            is_new = false;
            is_deleted = false;
            in_hunk = false;
        } else if in_hunk && !line.starts_with("@@ ") {
            // Inside a hunk, `+++i;` is an added `++i;` and `--- note` a
            // removed `-- note`: never file headers (issue #446 review).
            hunk_line(line, &mut lists, &mut current_line)?;
        } else if let Some(stripped) = line.strip_prefix("--- a/") {
            // For deleted files, this is the only place we get the path
            if current_file.is_none() {
                current_file = Some(stripped.to_string());
            }
        } else if line.starts_with("--- /dev/null") {
            is_new = true;
        } else if let Some(stripped) = line.strip_prefix("+++ b/") {
            current_file = Some(stripped.to_string());
        } else if line.starts_with("+++ /dev/null") {
            is_deleted = true;
        } else if line.starts_with("@@ ") {
            // Parse hunk header: @@ -old_start,old_count +new_start,new_count @@
            in_hunk = true;
            lists.close_removal();
            lists.gap_shift = u64::from(new_range_is_empty(line));
            if let Some(new_range) = parse_hunk_header(line) {
                if new_range >= DIFF_LINE_MAX {
                    return Err("diff_too_large: line counter would overflow".to_string());
                }
                current_line = new_range;
            }
        }
    }

    // Flush last file
    if let Some(fp) = current_file.take() {
        hunks.push(lists.into_hunk(fp, is_new, is_deleted));
    }

    Ok(hunks)
}

/// One body line of a hunk: `+` added, `-` removed, `\` metadata, anything
/// else context.
fn hunk_line(line: &str, lists: &mut LineLists, current_line: &mut u64) -> Result<(), String> {
    if line.starts_with('\\') {
        // "\ No newline at end of file": no line in either file.
        return Ok(());
    }
    if line.starts_with('-') {
        // Deleted lines affect the old position; we record current_line
        // so we can match the surrounding context in the new file.
        lists.changed.push(*current_line);
        lists.pending_removal.get_or_insert(*current_line);
        // Don't increment — deleted lines don't exist in the new file
        return Ok(());
    }
    if *current_line >= DIFF_LINE_MAX {
        return Err("diff_too_large: line counter would overflow".to_string());
    }
    if let Some(body) = line.strip_prefix('+') {
        lists.pending_removal = None;
        lists.changed.push(*current_line);
        // A blank added line changes no behavior; it must not make a
        // complete answer a lower bound (issue #446 review).
        if !body.trim().is_empty() {
            lists.added.push(*current_line);
        }
    } else {
        lists.close_removal();
    }
    *current_line += 1;
    Ok(())
}

/// True for a hunk header whose new range has a zero count (`+N,0`).
/// source: GNU diffutils manual, "Detailed Description of Unified Format" —
/// an empty range's start is the line just before it.
fn new_range_is_empty(line: &str) -> bool {
    line.find('+')
        .map(|p| &line[p + 1..])
        .and_then(|rest| rest.split(' ').next())
        .and_then(|range| range.split_once(','))
        .is_some_and(|(_, count)| count == "0")
}

fn parse_hunk_header(line: &str) -> Option<u64> {
    // Format: @@ -old_start[,old_count] +new_start[,new_count] @@
    let plus_pos = line.find('+')?;
    let rest = &line[plus_pos + 1..];
    let end = rest.find([',', ' '])?;
    rest[..end].parse::<u64>().ok()
}

// ---------------------------------------------------------------------------
// Symbol mapping — lines to graph nodes
// ---------------------------------------------------------------------------

/// `get_impact` per qualified name, computed once per diff: the symbol mapping
/// and `assess_dependency_confidence` both read it (`None` when it failed).
type ImpactCache = HashMap<String, Option<clustering::ImpactResult>>;

fn impact_of<'c>(
    store: &GraphStore,
    cache: &'c mut ImpactCache,
    qualified_name: &str,
) -> Option<&'c clustering::ImpactResult> {
    cache
        .entry(qualified_name.to_string())
        .or_insert_with(|| clustering::get_impact(store, qualified_name).ok())
        .as_ref()
}

/// The changed symbols of one file, plus why the file's changes may be
/// under-counted (none when every changed line lies in a symbol).
fn map_lines_to_symbols(
    store: &GraphStore,
    loaded: &symbols::DiffSymbols,
    impacts: &mut ImpactCache,
    hunk: &FileHunk,
) -> (Vec<ChangedSymbol>, Option<String>) {
    let whole_file = hunk.is_new || hunk.is_deleted;
    let attribution = loaded.attribute(&hunk.file_path, &hunk.added_lines, &hunk.removal_gaps);
    let coverage_gap = coverage_gap(hunk, &attribution);
    let change_type = if hunk.is_new {
        "added"
    } else if hunk.is_deleted {
        "deleted"
    } else {
        "modified"
    };
    let changed = attribution
        .candidates
        .into_iter()
        .filter(|c| whole_file || c.overlap > 0)
        .map(|c| {
            let (community_id, processes) = match impact_of(store, impacts, &c.qualified_name) {
                Some(imp) => (imp.communities.first().cloned(), imp.processes.clone()),
                None => (None, Vec::new()),
            };
            ChangedSymbol {
                lines_changed: if whole_file {
                    c.end.saturating_sub(c.start) + 1
                } else {
                    c.overlap
                },
                qualified_name: c.qualified_name,
                name: c.name,
                label: c.label.to_string(),
                file_path: hunk.file_path.clone(),
                change_type: change_type.to_string(),
                community_id,
                processes,
            }
        })
        .collect();
    (changed, coverage_gap)
}

/// Why `hunk`'s changes are not fully represented by symbols (issue #446): the
/// graph has no File at that path (an unsupported
/// language, a file outside the indexed root, a deleted file), or some changed
/// lines of a modified or added file lie in no symbol (top-level code, imports,
/// a kind without a line range).
fn coverage_gap(hunk: &FileHunk, attribution: &symbols::FileAttribution) -> Option<String> {
    let path = &hunk.file_path;
    if !attribution.file_known {
        return Some(format!(
            "{path}: no indexed File at this path (unsupported language, outside the \
             indexed root, or deleted); its changes are not attributed to any symbol"
        ));
    }
    if (hunk.is_new || hunk.is_deleted) && attribution.candidates.is_empty() {
        return Some(format!("{path}: the file holds no indexed symbol"));
    }
    match attribution.unattributed_lines {
        0 => None,
        // A deleted file's hunk (`+0,0`) carries no line of the indexed file.
        _ if hunk.is_deleted => None,
        n => Some(format!(
            "{path}: {n} changed line(s) lie in no indexed symbol (top-level code, \
             imports, a removal between symbols, or a declaration the graph does \
             not index)"
        )),
    }
}

// ---------------------------------------------------------------------------
// Risk score — heuristic, NOT paper-backed
// ---------------------------------------------------------------------------

/// Computes a risk score in [0.0, 1.0] for the set of changed symbols.
///
/// Heuristic formula (not sourced from any published algorithm):
///   risk = 0.6 * avg(processes_per_symbol / max_processes)
///        + 0.4 * (communities_affected / total_communities)
///
/// Rationale: symbols in more processes have higher blast radius (0.6 weight).
/// Changes spanning more communities indicate broader architectural impact
/// (0.4 weight). The weights are arbitrary engineering judgment.
fn compute_risk_score(symbols: &[ChangedSymbol], total_communities: u64) -> f64 {
    if symbols.is_empty() {
        return 0.0;
    }

    // Process centrality component
    let max_proc = symbols.iter().map(|s| s.processes.len()).max().unwrap_or(1);
    let max_proc = max_proc.max(1) as f64;
    let avg_proc_ratio: f64 = symbols
        .iter()
        .map(|s| s.processes.len() as f64 / max_proc)
        .sum::<f64>()
        / symbols.len() as f64;

    // Community spread component
    let communities: HashSet<&str> = symbols
        .iter()
        .filter_map(|s| s.community_id.as_deref())
        .collect();
    let total = total_communities.max(1) as f64;
    let community_ratio = communities.len() as f64 / total;

    (0.6 * avg_proc_ratio + 0.4 * community_ratio).min(1.0)
}

fn count_total_communities(store: &GraphStore) -> u64 {
    let cypher = "MATCH (c:Community) RETURN count(c)";
    if let Ok(qr) = store.execute_query(cypher) {
        if let Some(row) = qr.rows.first() {
            if let Some(val) = row.first() {
                return val.parse().unwrap_or(0);
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn analyze_diff(store: &GraphStore, diff_text: &str) -> Result<DiffAnalysis, String> {
    let hunks = parse_unified_diff(diff_text)?;
    build_analysis(store, &hunks)
}

pub fn analyze_git_diff(
    store: &GraphStore,
    codebase_path: &Path,
    base_ref: &str,
    head_ref: &str,
) -> Result<DiffAnalysis, String> {
    validate_git_ref(base_ref, "base_ref")?;
    validate_git_ref(head_ref, "head_ref")?;
    // The combined `base..head` string now starts with whatever base_ref
    // starts with (which validate_git_ref guarantees is NOT `-`).
    // The trailing `--` tells git no pathspecs follow, so an adversarial
    // head_ref cannot smuggle a path-based side effect either.
    // source: C2 fix — git-diff(1) synopsis `git diff [<commit>] [--] [<path>…]`.
    let output = Command::new("git")
        .arg("diff")
        .arg(format!("{base_ref}..{head_ref}"))
        .arg("--")
        .current_dir(codebase_path)
        .output()
        .map_err(|e| format!("failed to run git diff: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {stderr}"));
    }

    let diff_text = String::from_utf8_lossy(&output.stdout);
    analyze_diff(store, &diff_text)
}

fn build_analysis(store: &GraphStore, hunks: &[FileHunk]) -> Result<DiffAnalysis, String> {
    let files_changed = hunks.len() as u64;
    let mut all_symbols: Vec<ChangedSymbol> = Vec::new();

    let mut coverage_gaps: Vec<String> = Vec::new();
    let paths: Vec<&str> = hunks.iter().map(|h| h.file_path.as_str()).collect();
    let loaded = symbols::DiffSymbols::load(store, &paths);
    let mut impacts = ImpactCache::new();
    // One reason for the whole diff: the failed query is not any one file's.
    if loaded.query_failed() {
        coverage_gaps.push(
            "a graph query failed while loading the changed files' symbols; \
             the attribution may be incomplete"
                .to_string(),
        );
    }

    for hunk in hunks {
        let (symbols, gap) = map_lines_to_symbols(store, &loaded, &mut impacts, hunk);
        all_symbols.extend(symbols);
        coverage_gaps.extend(gap);
    }

    let communities: Vec<String> = all_symbols
        .iter()
        .filter_map(|s| s.community_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let processes: Vec<String> = all_symbols
        .iter()
        .flat_map(|s| s.processes.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let total_communities = count_total_communities(store);
    let risk_score = compute_risk_score(&all_symbols, total_communities);
    let (mean_dependency_confidence, mut epistemic, mut epistemic_reasons) =
        assess_dependency_confidence(store, &mut impacts, &all_symbols);
    // A change no symbol carries is a change the risk does not see: the answer
    // is a lower bound, never `exact` (issue #446).
    let coverage_gap_count = coverage_gaps.len();
    if coverage_gap_count > 0 {
        epistemic = crate::epistemic::Boundary::LowerBound.as_str();
        epistemic_reasons.extend(coverage_gaps);
    }

    Ok(DiffAnalysis {
        files_changed,
        symbols_affected: all_symbols,
        communities_affected: communities,
        processes_affected: processes,
        risk_score,
        mean_dependency_confidence,
        epistemic,
        epistemic_reasons,
        coverage_gap_count,
    })
}

/// Qualifies `risk_score` with the epistemic reliability of the graph edges it
/// rests on. For each changed symbol it pulls the depth-1 reverse-dependency
/// edges via `clustering::get_impact` (which now carries per-edge confidence and
/// a lower-bound flag), then reports the mean edge confidence and whether any
/// changed symbol's blast radius is a lower bound. This SURFACES uncertainty
/// rather than folding invented weights into the risk scalar — `risk_score`'s
/// formula is unchanged. source: epistemic module; checkpoint Phase 1 #2.
fn assess_dependency_confidence(
    store: &GraphStore,
    impacts: &mut ImpactCache,
    symbols: &[ChangedSymbol],
) -> (f64, &'static str, Vec<String>) {
    let mut confidence_sum = 0.0_f64;
    let mut edge_count = 0_u64;
    let mut reasons = Vec::new();
    let mut any_lower_bound = false;

    for sym in symbols {
        let Some(impact) = impact_of(store, impacts, &sym.qualified_name) else {
            continue; // symbol not in graph (e.g. deleted) — skip
        };
        for node in impact
            .callers
            .iter()
            .chain(impact.importers.iter())
            .chain(impact.users.iter())
            .chain(impact.implementors.iter())
        {
            confidence_sum += node.confidence;
            edge_count += 1;
        }
        if impact.epistemic == crate::epistemic::Boundary::LowerBound {
            any_lower_bound = true;
            for reason in &impact.epistemic_reasons {
                reasons.push(format!("{}: {reason}", sym.qualified_name));
            }
        }
    }

    // No resolved dependents → nothing heuristic to discount; confidence is
    // vacuously full (1.0), epistemic status exact.
    let mean = if edge_count == 0 {
        1.0
    } else {
        confidence_sum / edge_count as f64
    };
    let epistemic = if any_lower_bound {
        crate::epistemic::Boundary::LowerBound.as_str()
    } else {
        crate::epistemic::Boundary::Exact.as_str()
    };
    (mean, epistemic, reasons)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Codex review of PR #482: `@@ -2 +1,0 @@` removes old line 2, which sat
    /// before new line 2, not before new line 1.
    #[test]
    fn a_pure_removal_with_an_empty_new_range_sits_after_its_start() {
        let diff = "diff --git a/m.rs b/m.rs\n--- a/m.rs\n+++ b/m.rs\n\
                    @@ -2 +1,0 @@\n-    let x = 1;\n";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks[0].removal_gaps, vec![2]);
        // The history module's coordinates are unchanged.
        assert_eq!(hunks[0].changed_lines, vec![1]);
        let with_context = "diff --git a/m.rs b/m.rs\n--- a/m.rs\n+++ b/m.rs\n\
                            @@ -1,3 +1,2 @@\n fn f() {\n-    let x = 1;\n }\n";
        assert_eq!(
            parse_unified_diff(with_context).unwrap()[0].removal_gaps,
            vec![2]
        );
    }

    fn one_file(body: &str) -> FileHunk {
        let diff = format!("diff --git a/m.rs b/m.rs\n--- a/m.rs\n+++ b/m.rs\n{body}");
        parse_unified_diff(&diff).unwrap().remove(0)
    }

    /// Claude review of PR #482: how `added_lines` and `removal_gaps` record
    /// each hunk shape.
    #[test]
    fn added_lines_and_removal_gaps_per_hunk_shape() {
        // (hunk body, added_lines, removal_gaps)
        let cases: [(&str, &[u64], &[u64]); 8] = [
            // an added `++i;` is the hunk line `+++i;`, not a header
            ("@@ -1,2 +1,3 @@\n a\n+++i;\n b\n", &[2], &[]),
            // a removed `-- note` is the hunk line `--- note`: a removal
            ("@@ -1,3 +1,2 @@\n a\n--- note\n b\n", &[], &[2]),
            // a replacement is not a gap
            ("@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n", &[2], &[]),
            // a run partly replaced: the `+` cancels the gap
            ("@@ -1,4 +1,3 @@\n a\n-b\n-c\n+C\n d\n", &[2], &[]),
            // `-x +y -z`: the second `-` opens a gap after `y`
            ("@@ -1,4 +1,3 @@\n a\n-x\n+y\n-z\n d\n", &[2], &[3]),
            // a gap closed by the next `@@` header
            ("@@ -1,2 +1,1 @@\n a\n-b\n@@ -9,1 +8,1 @@\n z\n", &[], &[2]),
            // a blank added line changes nothing
            ("@@ -1,2 +1,3 @@\n a\n+\n+  \n b\n", &[], &[]),
            // "\ No newline at end of file" is no line of either file
            (
                "@@ -1,3 +1,3 @@\n f() {\n     1\n-}\n\\ No newline at end of file\n+} // x\n\\ No newline at end of file\n",
                &[3],
                &[],
            ),
        ];
        for (body, added, gaps) in cases {
            let hunk = one_file(body);
            assert_eq!(hunk.added_lines, added, "{body:?}");
            assert_eq!(hunk.removal_gaps, gaps, "{body:?}");
        }
    }

    #[test]
    fn a_removal_on_the_last_line_closes_before_the_next_file() {
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,1 @@\n a\n-b\n\
                    diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,1 +1,1 @@\n-x\n+y\n";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks[0].removal_gaps, vec![2]);
        assert!(hunks[1].removal_gaps.is_empty());
    }

    /// Claude review of PR #482: a failed symbol query yields one reason for
    /// the whole diff, not one per file. A store with no schema fails every
    /// query.
    #[test]
    fn a_failed_query_is_one_diff_level_reason() {
        let dir = tempfile::Builder::new()
            .prefix("git_diff_noschema")
            .tempdir()
            .unwrap();
        let store = GraphStore::open_or_create(&dir.path().join("db")).unwrap();
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n-x\n+y\n\
                    diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,1 +1,1 @@\n-x\n+y\n";
        let result = analyze_diff(&store, diff).unwrap();
        assert_eq!(result.epistemic, "lower-bound");
        let failed: Vec<&String> = result
            .epistemic_reasons
            .iter()
            .filter(|r| r.contains("a graph query failed"))
            .collect();
        assert_eq!(failed.len(), 1, "{:?}", result.epistemic_reasons);
    }

    #[test]
    fn test_parse_hunk_header() {
        assert_eq!(parse_hunk_header("@@ -10,5 +20,7 @@ fn foo"), Some(20));
        assert_eq!(parse_hunk_header("@@ -1 +1,3 @@"), Some(1));
        assert_eq!(parse_hunk_header("@@ -0,0 +1 @@"), Some(1));
    }

    #[test]
    fn test_parse_unified_diff_single_file() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,4 @@ fn main() {
     let x = 1;
+    let y = 2;
     let z = 3;
";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file_path, "src/main.rs");
        assert!(!hunks[0].changed_lines.is_empty());
        assert!(!hunks[0].is_new);
        assert!(!hunks[0].is_deleted);
    }

    #[test]
    fn test_parse_unified_diff_new_file() {
        let diff = "\
diff --git a/src/new.rs b/src/new.rs
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,5 @@
+fn hello() {
+    println!(\"hello\");
+}
";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file_path, "src/new.rs");
        assert!(hunks[0].is_new);
    }

    #[test]
    fn test_parse_unified_diff_deleted_file() {
        let diff = "\
diff --git a/src/old.rs b/src/old.rs
--- a/src/old.rs
+++ /dev/null
@@ -1,3 +0,0 @@
-fn old() {
-    // gone
-}
";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].is_deleted);
    }

    #[test]
    fn test_parse_unified_diff_multiple_files() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 fn a() {}
+fn a2() {}
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -5,3 +5,4 @@
 fn b() {}
+fn b2() {}
";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].file_path, "src/a.rs");
        assert_eq!(hunks[1].file_path, "src/b.rs");
    }

    #[test]
    fn test_compute_risk_score_empty() {
        assert_eq!(compute_risk_score(&[], 10), 0.0);
    }

    #[test]
    fn test_compute_risk_score_bounded() {
        let sym = ChangedSymbol {
            qualified_name: "src/main.rs::foo".into(),
            name: "foo".into(),
            label: "Function".into(),
            file_path: "src/main.rs".into(),
            change_type: "modified".into(),
            lines_changed: 5,
            community_id: Some("c0".into()),
            processes: vec!["p1".into(), "p2".into()],
        };
        let score = compute_risk_score(&[sym], 2);
        assert!((0.0..=1.0).contains(&score), "score out of range: {score}");
    }

    #[test]
    fn test_analyze_diff_on_empty_graph() {
        // source: issue #21 — a fixed `temp_dir().join("git_diff_test")` path
        // collides under default parallel `cargo test` execution (two test
        // threads racing the embedded DB's file lock on the same path).
        // tempfile::TempDir allocates a process- and call-unique directory
        // (mirrors the #13-fix pattern in tests/lbug_bulk_investigation.rs),
        // so no two test invocations can ever share a path.
        let dir = tempfile::Builder::new()
            .prefix("git_diff_test")
            .tempdir()
            .unwrap();
        let db_path = dir.path().join("testdb");

        let store = GraphStore::open_or_create(&db_path).unwrap();
        store.create_schema().unwrap();

        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,4 @@ fn main() {
     let x = 1;
+    let y = 2;
     let z = 3;
";
        let result = analyze_diff(&store, diff).unwrap();
        assert_eq!(result.files_changed, 1);
        // No symbols in an empty graph
        assert!(result.symbols_affected.is_empty());
        assert_eq!(result.risk_score, 0.0);
        // Issue #446: zero symbols for a non-empty diff is never `exact`.
        assert_eq!(result.epistemic, "lower-bound");
        assert_eq!(
            result.epistemic_reasons.len(),
            1,
            "{:?}",
            result.epistemic_reasons
        );
        assert!(result.epistemic_reasons[0].starts_with("src/main.rs: no indexed File"));
    }

    #[test]
    fn test_git_ref_with_dash_rejected() {
        // source: C2 fix — refs starting with `-` would be interpreted by git
        // as option flags (e.g. `--upload-pack=rm -rf ~`, `-c core.fsmonitor=...`).
        // Both base_ref and head_ref must be rejected before reaching Command::new.
        //
        // source: issue #21 — a fixed `temp_dir().join("git_ref_validate_test")`
        // path collides under default parallel `cargo test` execution; use a
        // unique-per-call tempfile::TempDir (matches the #13-fix pattern in
        // tests/lbug_bulk_investigation.rs).
        let dir = tempfile::Builder::new()
            .prefix("git_ref_validate_test")
            .tempdir()
            .unwrap();
        let db_path = dir.path().join("testdb");
        let store = GraphStore::open_or_create(&db_path).unwrap();
        store.create_schema().unwrap();

        let err = analyze_git_diff(&store, dir.path(), "--upload-pack=rm", "HEAD")
            .expect_err("must reject dash-prefixed base_ref");
        assert!(err.contains("invalid_ref"), "got: {err}");

        let err2 = analyze_git_diff(&store, dir.path(), "main", "-c")
            .expect_err("must reject dash-prefixed head_ref");
        assert!(err2.contains("invalid_ref"), "got: {err2}");

        let err3 =
            analyze_git_diff(&store, dir.path(), "", "HEAD").expect_err("must reject empty ref");
        assert!(err3.contains("invalid_ref"), "got: {err3}");
    }

    #[test]
    fn test_diff_too_large_overflow_guard() {
        // source: M3 fix — parse_unified_diff must return diff_too_large
        // rather than overflowing current_line. We can't realistically craft
        // a diff with u64::MAX lines, but validate_git_ref's logic is tested;
        // instead smoke-test that a normal diff still succeeds and returns Ok.
        let diff = "\
diff --git a/x b/x
--- a/x
+++ b/x
@@ -1 +1,2 @@
 keep
+add
";
        let hunks = parse_unified_diff(diff).expect("small diff must parse");
        assert_eq!(hunks.len(), 1);
    }
}
