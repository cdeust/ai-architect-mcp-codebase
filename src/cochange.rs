// cochange — Tornhill temporal-coupling mining (issue #58).
//
// Layer: application/analysis. Mines `git log` for FILE co-change and writes
// `FILE_CHANGES_WITH` File→File edges whose properties carry the coupling
// strength, so the architect agent's churning-pairs detection reads it straight
// from the graph. Pure git shell-out + graph writes; no parsing/AST.
//
// Metric (Tornhill 2015, *Your Code as a Crime Scene* — temporal coupling): for
// a file pair (a, b), `coupling = cochange_count / min(changes_a, changes_b)` —
// the fraction of the weaker file's revisions that also touched the other. We
// also store the symmetric `jaccard = cochange / (changes_a + changes_b -
// cochange)` and the most recent co-change timestamp.
//
// Thresholds — sourced from DeusData/codebase-memory-mcp `pass_githistory.c`
// (verified), which itself implements Tornhill's defaults:
//   * window `--since="1 year ago" --max-count=10000` — recent evolution is what
//     predicts co-change (older history is stale signal);
//   * skip commits touching > 20 files — merge/refactor/format sweeps are noise,
//     not coupling;
//   * require >= 3 co-changes AND coupling >= 0.30 (CBM `GH_MIN_COMMITS=3`,
//     `MIN_COUPLING_SCORE=0.3`; Tornhill's ~30% degree-of-coupling default).
//
// Incremental: git history is append-only, so an incremental pass EXTENDS the
// mined aggregates with only the commits since the last-mined sha
// (`<last_sha>..HEAD`) instead of re-walking the window, then re-derives the
// edge set from the extended aggregates. `mode = Full` re-mines the window from
// scratch. The aggregates + last-mined sha persist in a `cochange.json` sidecar.

use crate::graph_store::GraphStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Sidecar schema version.
pub const COCHANGE_SCHEMA_VERSION: u32 = 1;
/// Sidecar filename beside the graph in `output_dir`.
pub const COCHANGE_FILE: &str = "cochange.json";

// -- Thresholds (see module header for sources) -----------------------------
/// source: CBM pass_githistory.c `--since="1 year ago"`.
const WINDOW_SINCE: &str = "1 year ago";
/// source: CBM pass_githistory.c `--max-count=10000`.
const MAX_COMMITS: &str = "10000";
/// source: CBM `GH_MAX_FILES = 20` — skip merge/refactor sweeps.
const MAX_FILES_PER_COMMIT: usize = 20;
/// source: CBM `GH_MIN_COMMITS = 3`.
const MIN_COCHANGE: i64 = 3;
/// source: CBM `MIN_COUPLING_SCORE = 0.3` / Tornhill ~30% degree of coupling.
const MIN_COUPLING: f64 = 0.30;

/// Whether to re-mine the whole window or extend from the last-mined sha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Re-mine the window from scratch (used on a full index / `full: true`).
    Full,
    /// Extend the aggregates with commits since the last-mined sha.
    Incremental,
}

/// Aggregated per-pair co-change count + most recent co-change timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairAgg {
    pub count: i64,
    pub last_ts: i64,
}

/// The persisted mining state: per-file change counts, per-pair co-change
/// aggregates, and the HEAD sha the aggregates are current as of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CochangeState {
    pub schema_version: u32,
    /// HEAD sha the aggregates reflect (the incremental resume point).
    pub last_sha: String,
    /// file id → number of window commits that touched it.
    pub file_changes: BTreeMap<String, i64>,
    /// "a\u{1f}b" (a < b) → aggregate. The `\u{1f}` unit separator can't appear
    /// in a path, so it is an unambiguous pair key.
    pub pairs: BTreeMap<String, PairAgg>,
}

impl CochangeState {
    fn new() -> Self {
        CochangeState {
            schema_version: COCHANGE_SCHEMA_VERSION,
            last_sha: String::new(),
            file_changes: BTreeMap::new(),
            pairs: BTreeMap::new(),
        }
    }
}

/// Outcome of a mining pass, surfaced in the tool response.
#[derive(Debug, Clone)]
pub struct MineResult {
    pub mode: &'static str,
    pub commits_scanned: usize,
    pub edges_written: u64,
    pub mined_to_sha: String,
}

/// The cochange sidecar path for a tool `output_dir`.
pub fn state_path(output_dir: &Path) -> PathBuf {
    output_dir.join(COCHANGE_FILE)
}

/// Loads the sidecar; `None` when absent/unreadable/newer-schema (→ the caller
/// mines Full).
pub fn load_state(path: &Path) -> Option<CochangeState> {
    let bytes = std::fs::read(path).ok()?;
    let state: CochangeState = serde_json::from_slice(&bytes).ok()?;
    if state.schema_version > COCHANGE_SCHEMA_VERSION {
        return None;
    }
    Some(state)
}

fn save_state(path: &Path, state: &CochangeState) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(state).map_err(|e| format!("cochange: encode: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| format!("cochange: write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cochange: rename: {e}")
    })
}

/// Mines temporal coupling into `store` and refreshes the sidecar at
/// `state_path(output_dir)`.
///
/// Preconditions: `codebase` is a git working tree whose root equals the indexed
/// root (File ids line up with git paths; a subtree prefix is stripped). `store`
/// holds the indexed graph (its File nodes are the coupling universe).
/// Postconditions on `Ok`: `FILE_CHANGES_WITH` edges reflect the current mined
/// aggregates (thresholded), and the sidecar records the aggregates + HEAD sha.
/// `Mode::Full` rebuilds the aggregates from the window; `Mode::Incremental`
/// extends the prior aggregates with commits since `prior.last_sha` (falling back
/// to Full when there is no usable resume point). Returns `Ok(None)` when the
/// codebase is not a git working tree (nothing to mine — not an error).
pub fn mine(
    store: &GraphStore,
    codebase: &Path,
    output_dir: &Path,
    mode: Mode,
    prior: Option<CochangeState>,
) -> Result<Option<MineResult>, String> {
    let head = match git_head(codebase) {
        Some(h) => h,
        None => return Ok(None), // not a git tree
    };
    let prefix = git_output(codebase, &["rev-parse", "--show-prefix"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    // Decide the git range and the starting aggregates.
    let (mut state, range_args, effective_mode): (CochangeState, Vec<String>, &'static str) =
        match (mode, prior) {
            (Mode::Incremental, Some(p))
                if !p.last_sha.is_empty() && git_known(codebase, &p.last_sha) =>
            {
                // Extend: only commits after the last-mined sha.
                (
                    p.clone(),
                    vec![format!("{}..HEAD", p.last_sha)],
                    "incremental",
                )
            }
            _ => {
                // Full: re-mine the window from scratch.
                (
                    CochangeState::new(),
                    vec![
                        format!("--since={WINDOW_SINCE}"),
                        format!("--max-count={MAX_COMMITS}"),
                    ],
                    "full",
                )
            }
        };

    let commits = read_commit_files(codebase, &range_args, &prefix)?;
    let commits_scanned = commits.len();
    accumulate(&mut state, &commits);
    state.last_sha = head.clone();

    let edges_written = rewrite_edges(store, &state)?;
    save_state(&state_path(output_dir), &state)?;

    Ok(Some(MineResult {
        mode: effective_mode,
        commits_scanned,
        edges_written,
        mined_to_sha: head,
    }))
}

/// One commit's changed files (already stripped to indexed-subtree ids) + the
/// commit timestamp.
struct CommitFiles {
    files: Vec<String>,
    ts: i64,
}

/// Folds each commit's file set into the per-file and per-pair aggregates.
/// Commits touching more than `MAX_FILES_PER_COMMIT` files are skipped as noise.
fn accumulate(state: &mut CochangeState, commits: &[CommitFiles]) {
    for c in commits {
        if c.files.len() > MAX_FILES_PER_COMMIT || c.files.len() < 2 {
            // > cap: merge/refactor noise. < 2: no pair to couple.
            // A single-file commit still counts toward that file's change total.
            if c.files.len() == 1 {
                *state.file_changes.entry(c.files[0].clone()).or_insert(0) += 1;
            }
            continue;
        }
        for f in &c.files {
            *state.file_changes.entry(f.clone()).or_insert(0) += 1;
        }
        // Unordered pairs, keyed with a < b.
        for i in 0..c.files.len() {
            for j in (i + 1)..c.files.len() {
                let (a, b) = order(&c.files[i], &c.files[j]);
                let key = pair_key(a, b);
                let agg = state.pairs.entry(key).or_insert(PairAgg {
                    count: 0,
                    last_ts: 0,
                });
                agg.count += 1;
                if c.ts > agg.last_ts {
                    agg.last_ts = c.ts;
                }
            }
        }
    }
}

/// Drops every `FILE_CHANGES_WITH` edge and re-inserts one per pair that clears
/// the thresholds and whose BOTH endpoints are File nodes in the graph. Returns
/// the number of edges written. Re-deriving wholesale keeps the incremental and
/// full paths identical downstream — only the aggregates differ.
fn rewrite_edges(store: &GraphStore, state: &CochangeState) -> Result<u64, String> {
    store.execute_query("MATCH ()-[r:FILE_CHANGES_WITH]->() DELETE r")?;
    let mut written = 0u64;
    for (key, agg) in &state.pairs {
        if agg.count < MIN_COCHANGE {
            continue;
        }
        let (a, b) = match split_key(key) {
            Some(pair) => pair,
            None => continue,
        };
        let ca = *state.file_changes.get(a).unwrap_or(&0);
        let cb = *state.file_changes.get(b).unwrap_or(&0);
        let min = ca.min(cb);
        if min <= 0 {
            continue;
        }
        let coupling = agg.count as f64 / min as f64;
        if coupling < MIN_COUPLING {
            continue;
        }
        let union = ca + cb - agg.count;
        let jaccard = if union > 0 {
            agg.count as f64 / union as f64
        } else {
            0.0
        };
        let props: Vec<(&str, String)> = vec![
            ("cochange_count", agg.count.to_string()),
            ("support", min.to_string()),
            ("coupling", format!("{coupling:.4}")),
            ("jaccard", format!("{jaccard:.4}")),
            ("last_co_change", agg.last_ts.to_string()),
        ];
        let prop_refs: Vec<(&str, &str)> = props.iter().map(|(k, v)| (*k, v.as_str())).collect();
        // Only couple files that are actually in the graph (both File nodes
        // exist); insert_edge's MATCH…MATCH…CREATE is a no-op otherwise.
        match store.insert_edge("FILE_CHANGES_WITH", a, b, &prop_refs) {
            Ok(()) => {
                // `insert_edge`'s MATCH…MATCH…CREATE is a silent no-op when
                // either endpoint is absent, so a successful call is not proof
                // the edge landed — probe for it. The endpoint labels come from
                // REL_TABLES (`FILE_CHANGES_WITH` is File -> File), which is why
                // this uses the store's own `edge_exists` rather than a
                // second, hand-written File/File probe.
                if store
                    .edge_exists("FILE_CHANGES_WITH", a, b)
                    .unwrap_or(false)
                {
                    written += 1;
                }
            }
            Err(e) => eprintln!("cochange: edge {a} <-> {b} skipped: {e}"),
        }
    }
    Ok(written)
}

// -- git plumbing -----------------------------------------------------------

/// Runs `git -C codebase log <range> --no-merges --name-only` and returns the
/// per-commit file lists (stripped to the indexed subtree). Paths outside the
/// subtree `prefix` are dropped.
fn read_commit_files(
    codebase: &Path,
    range_args: &[String],
    prefix: &str,
) -> Result<Vec<CommitFiles>, String> {
    // Record separator \u{1e} starts each commit header; unit separator \u{1f}
    // delimits sha/timestamp. Neither can appear in a path or these fields.
    let mut args: Vec<String> = vec![
        "-c".into(),
        "core.quotepath=false".into(),
        "log".into(),
        "--no-merges".into(),
        "--name-only".into(),
        "--pretty=format:\u{1e}%H\u{1f}%at".into(),
    ];
    args.extend(range_args.iter().cloned());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = match git_output(codebase, &arg_refs) {
        Some(s) => s,
        None => return Ok(Vec::new()), // empty range / no commits
    };
    let mut commits = Vec::new();
    for chunk in out.split('\u{1e}') {
        let chunk = chunk.trim_matches('\n');
        if chunk.is_empty() {
            continue;
        }
        let mut lines = chunk.lines();
        let header = match lines.next() {
            Some(h) => h,
            None => continue,
        };
        let ts = header
            .split('\u{1f}')
            .nth(1)
            .and_then(|t| t.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let files: Vec<String> = lines
            .filter(|l| !l.trim().is_empty())
            .filter_map(|p| strip_prefix_path(p, prefix))
            .collect();
        if !files.is_empty() {
            commits.push(CommitFiles { files, ts });
        }
    }
    Ok(commits)
}

fn strip_prefix_path(path: &str, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return Some(path.to_string());
    }
    path.strip_prefix(prefix).map(|s| s.to_string())
}

fn git_output(codebase: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(codebase)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_head(codebase: &Path) -> Option<String> {
    let s = git_output(codebase, &["rev-parse", "HEAD"])?;
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// True when `sha` is an object this clone knows (so `<sha>..HEAD` is valid).
fn git_known(codebase: &Path, sha: &str) -> bool {
    sha.chars().all(|c| c.is_ascii_hexdigit())
        && git_output(codebase, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_some()
}

// -- pair-key helpers -------------------------------------------------------

fn order<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn pair_key(a: &str, b: &str) -> String {
    format!("{a}\u{1f}{b}")
}

fn split_key(key: &str) -> Option<(&str, &str)> {
    let mut it = key.split('\u{1f}');
    let a = it.next()?;
    let b = it.next()?;
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulate_counts_pairs_and_skips_noise() {
        let mut state = CochangeState::new();
        // Two commits changing (a,b); one big noisy commit; one single-file.
        let commits = vec![
            CommitFiles {
                files: vec!["a".into(), "b".into()],
                ts: 10,
            },
            CommitFiles {
                files: vec!["a".into(), "b".into()],
                ts: 20,
            },
            CommitFiles {
                // > MAX_FILES_PER_COMMIT → skipped entirely.
                files: (0..25).map(|i| format!("f{i}")).collect(),
                ts: 30,
            },
            CommitFiles {
                files: vec!["a".into()],
                ts: 40,
            },
        ];
        accumulate(&mut state, &commits);
        assert_eq!(state.file_changes["a"], 3, "a changed in 3 counted commits");
        assert_eq!(state.file_changes["b"], 2);
        let key = pair_key("a", "b");
        assert_eq!(state.pairs[&key].count, 2);
        assert_eq!(state.pairs[&key].last_ts, 20);
        // The noisy 25-file commit contributed no pairs.
        assert!(!state.file_changes.contains_key("f0"));
    }

    #[test]
    fn thresholds_are_the_sourced_values() {
        assert_eq!(MIN_COCHANGE, 3);
        assert_eq!(MAX_FILES_PER_COMMIT, 20);
        assert!((MIN_COUPLING - 0.30).abs() < f64::EPSILON);
    }
}
