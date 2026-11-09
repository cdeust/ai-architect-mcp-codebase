// git_diff — maps git diff changed lines to affected symbols in the code graph.
//
// Two entry points:
//   1. analyze_diff(store, diff_text) — parse raw unified diff text
//   2. analyze_git_diff(store, codebase_path, base_ref, head_ref) — run git diff
//
// Each changed line is attributed to the innermost graph symbols whose
// [start_line, end_line] range holds it (`symbols`). Each affected symbol is
// enriched with community and process membership. A change no symbol holds (a
// line or a pure removal outside every symbol, blank-only ones excepted; a path
// with no indexed File; an added or deleted file with no symbol; an unreadable
// or binary section) or a failed symbol load makes the answer `lower-bound`
// (issue #446).

use crate::clustering;
use crate::graph_store::GraphStore;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

mod parse;
mod symbols;

#[cfg(test)]
use parse::parse_hunk_header;
pub(crate) use parse::{parse_unified_diff, FileHunk};

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
    /// `lower-bound` (issue #446) when part of a file's diff could not be
    /// read (malformed, truncated or path-less), a section is binary, a path
    /// has no indexed File, an added or deleted file holds no indexed symbol,
    /// a changed line or pure removal of a modified or added file lies in no
    /// indexed symbol (blank-only ones excepted), or the symbol load failed;
    /// or when a changed symbol's impact is itself a lower bound (dynamic
    /// dispatch, heuristic edges, unresolved call sites). `exact` otherwise.
    /// source: epistemic module.
    pub epistemic: &'static str,
    /// Carriers of epistemic uncertainty (empty when `exact`): a changed
    /// symbol and why its impact under-counts, a changed file and why some of
    /// its changes reach no symbol, or one diff-level reason when the query
    /// loading the changed files' symbols failed (issue #446).
    pub epistemic_reasons: Vec<String>,
    /// How many of `epistemic_reasons` are coverage gaps (a changed file whose
    /// changes partly or wholly reach no symbol, or a failed symbol load) rather than
    /// dependency uncertainty. Internal: read by `next_steps`, not emitted.
    #[serde(skip)]
    pub coverage_gap_count: usize,
}

// ---------------------------------------------------------------------------
// Symbol mapping — lines to graph nodes
// ---------------------------------------------------------------------------

/// `get_impact` per qualified name, computed once per diff: the symbol mapping
/// and `assess_dependency_confidence` both read it. `None` mirrors its `Err`
/// signature; it reports no inner query failure today.
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
    let attribution = loaded.attribute(hunk);
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
                    c.end.saturating_sub(c.start).saturating_add(1)
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

/// Why `hunk`'s changes are not fully represented by symbols (issue #446), in
/// this order: part of its diff could not be read; it is binary; the graph has
/// no File at that path (an unsupported language, a file outside the indexed
/// root, a deleted file); it is added or deleted and holds no symbol; or some
/// changed lines or pure removals of a modified or added file lie in no symbol
/// (blank-only ones excepted; a deleted file's lines are exempt).
fn coverage_gap(hunk: &FileHunk, attribution: &symbols::FileAttribution) -> Option<String> {
    let path = &hunk.file_path;
    if hunk.malformed {
        return Some(format!(
            "{path}: part of this file's diff could not be read (an unreadable or \
             truncated hunk, or a missing or repeated file header); some of its \
             changes may be missing"
        ));
    }
    if hunk.binary {
        return Some(format!(
            "{path}: binary content; its changes are not attributed to any symbol"
        ));
    }
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
    let hunks = merge_by_path(parse_unified_diff(diff_text)?);
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
        // Non-ASCII paths stay unquoted, so `+++ b/<path>` matches the File.
        .args(["-c", "core.quotePath=false"])
        .arg("diff")
        // The parser reads `a/`/`b/` paths and plain `+`/`-` lines: pin them
        // against `diff.noprefix`, `color.diff=always` and `diff.external`, under
        // which every file would be dropped (issue #446 review).
        // A rename would be a section with no hunk, and a textconv driver
        // would shift the line numbers away from the indexed file.
        .args([
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ])
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

/// One `FileHunk` per path: a diff that names a path several times (a crafted
/// `diff_text`, or `git diff` for a file<->symlink type change) is attributed
/// once, so it cannot
/// duplicate symbols, skew the risk averages or multiply the output.
fn merge_by_path(hunks: Vec<FileHunk>) -> Vec<FileHunk> {
    let mut merged: Vec<FileHunk> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for h in hunks {
        let Some(&i) = index.get(&h.file_path) else {
            index.insert(h.file_path.clone(), merged.len());
            merged.push(h);
            continue;
        };
        let m = &mut merged[i];
        m.changed_lines.extend(h.changed_lines);
        m.added_lines.extend(h.added_lines);
        m.removal_gaps.extend(h.removal_gaps);
        m.nonblank_lines.extend(h.nonblank_lines);
        m.nonblank_gaps.extend(h.nonblank_gaps);
        // Sections that disagree on the change type (one modified, another
        // added or deleted) cannot all be right.
        m.malformed |= h.malformed || m.is_new != h.is_new || m.is_deleted != h.is_deleted;
        m.binary |= h.binary;
        m.is_new |= h.is_new;
        m.is_deleted |= h.is_deleted;
    }
    merged
}

/// Every hunk's changed symbols into `out`; returns the coverage gaps.
fn map_all_hunks(
    store: &GraphStore,
    hunks: &[FileHunk],
    impacts: &mut ImpactCache,
    out: &mut Vec<ChangedSymbol>,
) -> Vec<String> {
    let mut coverage_gaps: Vec<String> = Vec::new();
    let paths: Vec<&str> = hunks.iter().map(|h| h.file_path.as_str()).collect();
    let loaded = symbols::DiffSymbols::load(store, &paths);
    // One reason for the whole diff: the failed query is not any one file's.
    if loaded.query_failed() {
        coverage_gaps.push(
            "a graph query failed while loading the changed files' symbols; \
             the attribution may be incomplete"
                .to_string(),
        );
    }
    for hunk in hunks {
        let (symbols, gap) = map_lines_to_symbols(store, &loaded, impacts, hunk);
        out.extend(symbols);
        coverage_gaps.extend(gap);
    }
    coverage_gaps
}

fn build_analysis(store: &GraphStore, hunks: &[FileHunk]) -> Result<DiffAnalysis, String> {
    let files_changed = hunks.len() as u64;
    let mut all_symbols: Vec<ChangedSymbol> = Vec::new();

    let mut impacts = ImpactCache::new();
    let coverage_gaps = map_all_hunks(store, hunks, &mut impacts, &mut all_symbols);

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
        // `get_impact` returns `Ok` even when its inner queries fail (they
        // yield empty lists), so `None` only stands for its `Err` signature.
        let Some(impact) = impact_of(store, impacts, &sym.qualified_name) else {
            continue;
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
        let cases: [(&str, &[u64], &[u64]); 10] = [
            // removing a blank line: a gap, marked blank (see below)
            ("@@ -1,3 +1,2 @@\n a\n-\n b\n", &[], &[2]),
            // Codex review of PR #482: a statement replaced by a blank line
            // is still a change at that line
            ("@@ -1,3 +1,3 @@\n f() {\n-    do_work();\n+\n }\n", &[2], &[]),
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
            // a gap closed by the next `@@` header (no context line after it)
            ("@@ -1,2 +1,1 @@\n a\n-b\n@@ -9,1 +8,1 @@\n-y\n+Y\n", &[8], &[2]),
            // blank added lines are recorded (and marked blank, see below)
            ("@@ -1,2 +1,4 @@\n a\n+\n+  \n b\n", &[2, 3], &[]),
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

    /// Only changes beyond blank lines are marked non-blank, so attribution
    /// can skip blank-only ones outside every symbol; a blank line replacing
    /// code is not blank-only.
    #[test]
    fn non_blank_changes_are_marked() {
        let h = one_file("@@ -1,4 +1,4 @@\n a\n+\n-\n b\n-x\n+\n");
        assert_eq!(h.added_lines, vec![2, 4]);
        assert_eq!(h.nonblank_lines, vec![4]);
        assert_eq!(h.removal_gaps, vec![3]);
        assert!(h.nonblank_gaps.is_empty());
    }

    /// Claude review of PR #482: real `diff -u` headers (no prefix, a tab and
    /// a timestamp), the `diff --git` path when no header names the file, and
    /// binary sections.
    #[test]
    fn headers_without_git_prefixes_still_name_the_file() {
        let plain = "--- old.rs\t2026-11-09 10:00:00.000000000 +0100\n\
                     +++ new.rs\t2026-11-09 10:01:00.000000000 +0100\n@@ -1,1 +1,1 @@\n-a\n+b\n";
        assert_eq!(parse_unified_diff(plain).unwrap()[0].file_path, "new.rs");
        let binary = "diff --git a/img.png b/img.png\nindex 1..2 100644\n\
                      Binary files a/img.png and b/img.png differ\n";
        let hunks = parse_unified_diff(binary).unwrap();
        assert_eq!(hunks[0].file_path, "img.png");
        assert!(hunks[0].binary);
        let headless = "@@ -1,1 +1,1 @@\n-a\n+b\n";
        let hunks = parse_unified_diff(headless).unwrap();
        assert!(hunks[0].malformed, "a hunk with no file path is reported");
        // Neither an unreadable headless hunk nor a binary section whose quoted
        // `diff --git` paths cannot be read disappears.
        let unreadable = parse_unified_diff("@@ -5,x +5,1 @@\n-old\n+new\n").unwrap();
        assert!(unreadable[0].malformed);
        let quoted = "diff --git \"a/x\\ty.png\" \"b/x\\ty.png\"\n\
                      Binary files \"a/x\\ty.png\" and \"b/x\\ty.png\" differ\n";
        let hunks = parse_unified_diff(quoted).unwrap();
        assert!(hunks[0].binary && hunks[0].malformed);
    }

    /// Claude review of PR #482: a hunk ends where its header's counts say,
    /// so header-like body lines stay in the file and a `git format-patch`
    /// trailer after the last hunk is no removal.
    #[test]
    fn a_hunk_ends_at_its_counts() {
        let h = one_file("@@ -1,3 +1,3 @@\n a\n--- a/x.rs\n+++ b/evil.rs\n c\n");
        assert_eq!(h.file_path, "m.rs");
        assert_eq!(h.added_lines, vec![2]);
        let h = one_file("@@ -1,2 +1,2 @@\n a\n-b\n+B\n-- \n2.43.0\n");
        assert!(h.removal_gaps.is_empty());
    }

    /// Claude review of PR #482: parser edge cases that must not yield a
    /// silent `exact`.
    #[test]
    fn parser_edge_cases_from_review() {
        // An empty `+++ ` path is no `/dev/null`: the section is unreadable,
        // not a deleted file.
        let h = &parse_unified_diff("--- a/m.rs\n+++ \n@@ -1,1 +1,1 @@\n-a\n+b\n").unwrap()[0];
        assert!(h.malformed && !h.is_deleted);
        // A path holding ` b/` (git does not quote spaces).
        let bin = "diff --git a/docs/a b/c.png b/docs/a b/c.png\nBinary files differ\n";
        assert_eq!(
            parse_unified_diff(bin).unwrap()[0].file_path,
            "docs/a b/c.png"
        );
        // The next commit of a `git log -p` is no stray hunk line.
        let log = "diff --git a/m.rs b/m.rs\n--- a/m.rs\n+++ b/m.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n\
                   commit 0123456789abcdef0123456789abcdef01234567\n\n    Fix bug\n";
        assert!(!parse_unified_diff(log).unwrap()[0].malformed);
        // A malformed first hunk does not hide the next file of a plain diff.
        let plain = "--- a/x.rs\n+++ b/x.rs\n@@ -1,x +1 @@\n-a\n+A\n\
                     --- a/y.rs\n+++ b/y.rs\n@@ -1,1 +1,1 @@\n-b\n+B\n";
        let paths: Vec<String> = parse_unified_diff(plain)
            .unwrap()
            .into_iter()
            .map(|h| h.file_path)
            .collect();
        assert_eq!(paths, vec!["x.rs", "y.rs"]);
        // A hunk-less section (mode change, empty new file) adds no file.
        let chmod = "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
        assert!(parse_unified_diff(chmod).unwrap().is_empty());
    }

    /// Claude review of PR #482: a hunk that cannot be read is flagged, not
    /// dropped silently; the format-patch signature is not.
    #[test]
    fn unreadable_hunks_are_flagged() {
        for body in [
            "@@ -5,x +5,1 @@\n-old\n+new\n",
            "@@ -1 +1 @@\n-a\n+A\n+B\n",
            "@@@ -1,2 -1,2 +1,3 @@@\n  a\n++b\n",
            // truncated: the header owes 4 new lines, the input ends first
            "@@ -10,3 +10,4 @@\n x\n",
            // a `+` line before any `@@`, after the file header
            "+fn evil() {}\n",
            // a second `+++` would rename the file
            "@@ -1,1 +1,1 @@\n-a\n+b\n+++ b/other.rs\n",
        ] {
            assert!(one_file(body).malformed, "{body:?}");
        }
        let signed = one_file("@@ -1,1 +1,1 @@\n-a\n+A\n-- \n2.43.0\n");
        assert!(!signed.malformed);
    }

    /// Plain `diff -u` output has no `diff --git` lines: each `---` header
    /// after a file's `+++` starts the next file.
    #[test]
    fn plain_unified_diffs_split_at_each_file_header() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,1 @@\n-a\n+A\n\
                    --- a/y.rs\n+++ b/y.rs\n@@ -1,1 +1,1 @@\n-b\n+B\n";
        let hunks = parse_unified_diff(diff).unwrap();
        let paths: Vec<&str> = hunks.iter().map(|h| h.file_path.as_str()).collect();
        assert_eq!(paths, vec!["x.rs", "y.rs"]);
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
@@ -10,2 +10,3 @@ fn main() {
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
@@ -1,1 +1,2 @@
 fn a() {}
+fn a2() {}
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -5,1 +5,2 @@
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
@@ -10,2 +10,3 @@ fn main() {
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
        // rather than overflowing current_line: a normal diff still parses,
        // and a hunk header at the cap is rejected (below).
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
        // A header at the cap is rejected instead of overflowing later.
        let huge = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +9223372036854775807 @@\n+a\n";
        let err = parse_unified_diff(huge).err().expect("header past the cap");
        assert!(err.starts_with("diff_too_large"), "{err}");
    }

    /// Claude review of PR #482: `analyze_diff` merges repeated paths.
    #[test]
    fn analyze_diff_counts_a_repeated_path_once() {
        let dir = tempfile::Builder::new()
            .prefix("git_diff_merge")
            .tempdir()
            .unwrap();
        let store = GraphStore::open_or_create(&dir.path().join("db")).unwrap();
        store.create_schema().unwrap();
        let section = "diff --git a/m.rs b/m.rs\n--- a/m.rs\n+++ b/m.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n";
        let result = analyze_diff(&store, &section.repeat(2)).unwrap();
        assert_eq!(result.files_changed, 1);
        assert_eq!(
            result.coverage_gap_count, 1,
            "{:?}",
            result.epistemic_reasons
        );
    }

    /// Claude review of PR #482: a path named twice is attributed once.
    #[test]
    fn a_path_named_twice_is_one_file() {
        let section = "diff --git a/m.rs b/m.rs\n--- a/m.rs\n+++ b/m.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n";
        let hunks = merge_by_path(parse_unified_diff(&section.repeat(3)).unwrap());
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].added_lines, vec![1, 1, 1]);
    }
}
