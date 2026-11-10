// git_diff::symbols — attributes a diff's changed lines to the graph symbols
// that contain them (issue #446).
//
// Three defects made `detect_changes` misreport a change (a modified method as
// its class or as nothing, a removal as the next symbol):
//
// - Methods were looked up through `Defines_File_Method`, a table that does not
//   exist (methods hang off their container through `HasMethod_*`; see
//   `search::impact_target`'s header). The query raised a binder error, the
//   error was skipped, and no Method was ever a candidate. A Rust `impl` method
//   whose type is defined in another file has no in-file container at all.
//   Every symbol node's `id` starts with `<File.id>::` (the prefix the
//   incremental purge, `indexer::incremental::mutate::purge_file_symbols`,
//   relies on), so every label is read by that prefix instead. This also
//   reaches the symbols whose `Defines` edge the persister drops (a C++
//   namespace's functions, a Rust inline `mod`'s functions, nested classes).
// - A changed line inside a method also lies inside its class, so once methods
//   are candidates it would count for both. Each line goes to the innermost
//   candidates whose range holds it; on equal ranges the deeper id (`A::m`
//   over `A`) wins, and siblings that neither range nor id can tell apart all
//   keep it.
// - A pure removal was credited to the line after it: deleting `fn foo` just
//   above `fn bar` reported `bar`. It now counts for a symbol only when that
//   symbol spans the lines on both sides of it.
//
// Each label is read once per chunk of 64 paths (one `starts_with` query per
// label and chunk), not once per file.
//
// Lines that fall in no candidate, and queries that fail, are reported, so the
// caller can refuse to call an incomplete answer `exact`.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::FileHunk;
use crate::graph_store::{cypher_str, GraphStore};

/// Symbol labels that carry a line range.
const LABELS_WITH_LINES: &[&str] = &[
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Trait",
    "Constant",
    "TypeAlias",
];

/// Prefixes OR-ed into one `starts_with` query: one scan per label serves a
/// whole chunk of files instead of one scan per file (the OR-of-prefixes
/// shape of `indexer::incremental::edges`, chunked here to bound the query).
const PREFIX_CHUNK: usize = 64;

/// One symbol of a file with its line range.
#[derive(Clone)]
pub(super) struct Candidate {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) qualified_name: String,
    pub(super) label: &'static str,
    pub(super) start: u64,
    pub(super) end: u64,
    /// Changed lines attributed to this candidate.
    pub(super) overlap: u64,
}

/// The symbols of every file a diff names, read once per chunk of paths.
pub(super) struct DiffSymbols {
    /// `File.id`s the graph holds among the diff's paths (`File.id` is the
    /// path: `indexer::persist::insert_file_node`).
    known_files: HashSet<String>,
    /// Indexed Files whose path extends a diff path with `::` (`a.rs::b.rs`
    /// next to `a.rs`): their symbols also match the `a.rs::` prefix and
    /// must not be credited to `a.rs` (Codex security review of PR #482).
    nested_files: Vec<String>,
    by_file: HashMap<String, Vec<Candidate>>,
    /// Whether a graph query failed, so the candidates may be incomplete.
    query_failed: bool,
}

/// The symbols of one file and how its changed lines distribute over them.
pub(super) struct FileAttribution {
    /// Whether the graph holds a `File` with this path.
    pub(super) file_known: bool,
    pub(super) candidates: Vec<Candidate>,
    /// Distinct changed lines (and pure removals) that lie in no candidate.
    pub(super) unattributed_lines: u64,
    /// `ATTRIBUTION_BUDGET` ran out: the later lines were not attributed.
    pub(super) budget_exceeded: bool,
}

/// Most (line, holding symbol) pairs one file's attribution visits. A sweep
/// visits each pair once, so real files stay far below it; it bounds only
/// pathological nesting (thousands of symbols each spanning thousands of
/// changed lines; Codex security review of PR #482).
const ATTRIBUTION_BUDGET: usize = 4_000_000;

impl DiffSymbols {
    /// Whether a File or symbol query failed while loading, so some
    /// candidates may be missing.
    pub(super) fn query_failed(&self) -> bool {
        self.query_failed
    }

    /// Reads the Files at `paths` and every line-ranged symbol whose id starts
    /// with `<File.id>::`, one query per label and chunk of paths.
    pub(super) fn load(store: &GraphStore, paths: &[&str]) -> Self {
        let mut loaded = DiffSymbols {
            known_files: HashSet::new(),
            nested_files: Vec::new(),
            by_file: HashMap::new(),
            query_failed: false,
        };
        let mut unique: Vec<&str> = paths.to_vec();
        unique.sort_unstable();
        unique.dedup();
        for chunk in unique.chunks(PREFIX_CHUNK) {
            loaded.load_files(store, chunk);
            let known: Vec<&str> = chunk
                .iter()
                .copied()
                .filter(|p| loaded.known_files.contains(*p))
                .collect();
            if !known.is_empty() {
                for &label in LABELS_WITH_LINES {
                    loaded.load_label(store, label, &known);
                }
            }
        }
        loaded
    }

    fn load_files(&mut self, store: &GraphStore, chunk: &[&str]) {
        let cond = or_of(chunk, |p| {
            format!(
                "f.id = {} OR starts_with(f.id, {})",
                cypher_str(p),
                cypher_str(&format!("{p}::"))
            )
        });
        let Ok(qr) = store.execute_query(&format!("MATCH (f:File) WHERE {cond} RETURN f.id"))
        else {
            self.query_failed = true;
            return;
        };
        for id in qr.rows.into_iter().filter_map(|r| r.into_iter().next()) {
            if chunk.contains(&id.as_str()) {
                self.known_files.insert(id);
            } else {
                self.nested_files.push(id);
            }
        }
    }

    fn load_label(&mut self, store: &GraphStore, label: &'static str, files: &[&str]) {
        let cond = or_of(files, |f| {
            format!("starts_with(n.id, {})", cypher_str(&format!("{f}::")))
        });
        let cypher = format!(
            "MATCH (n:{label}) WHERE {cond} \
             RETURN n.id, n.name, n.qualified_name, n.start_line, n.end_line"
        );
        let Ok(qr) = store.execute_query(&cypher) else {
            self.query_failed = true;
            return;
        };
        for row in qr.rows {
            let Ok([id, name, qualified_name, start, end]) = <[String; 5]>::try_from(row) else {
                continue;
            };
            let Some(file) = owning_file(files, &id) else {
                continue;
            };
            // A longer indexed File owns it (`a.rs::b.rs::f`, not `a.rs`).
            let nested = self.nested_files.iter().any(|n| {
                n.len() > file.len()
                    && id
                        .strip_prefix(n.as_str())
                        .is_some_and(|rest| rest.starts_with("::"))
            });
            if nested {
                continue;
            }
            let start: u64 = start.parse().unwrap_or(0);
            let end: u64 = end.parse().unwrap_or(0);
            self.by_file
                .entry(file.to_string())
                .or_default()
                .push(Candidate {
                    id,
                    name,
                    qualified_name,
                    label,
                    start,
                    end: end.max(start),
                    overlap: 0,
                });
        }
    }

    /// Attributes `hunk`'s added lines and pure removals to its file's
    /// symbols. A blank-only line or removal outside every symbol is no gap;
    /// inside a symbol it still counts (it can change a multiline literal).
    ///
    /// postcondition: every distinct line and gap counts in
    /// `unattributed_lines`, in the `overlap` of at least one candidate (never
    /// one that encloses another holding it), or is a blank-only change
    /// outside every symbol.
    pub(super) fn attribute(&self, hunk: &FileHunk) -> FileAttribution {
        let mut candidates = self
            .by_file
            .get(&hunk.file_path)
            .cloned()
            .unwrap_or_default();
        let nonblank_lines: HashSet<u64> = hunk.nonblank_lines.iter().copied().collect();
        let nonblank_gaps: HashSet<u64> = hunk.nonblank_gaps.iter().copied().collect();
        // Every query is `from..=to`: a line (`l..=l`) or a removal between
        // lines p-1 and p (`p-1..=p`). A removal belongs to a symbol only when
        // that symbol spans both: deleting `fn foo` just above `fn bar` is not
        // a change of `bar`. Line 0 does not exist, so a gap at 1 is outside.
        // Sorted by (from, to), both bounds never decrease, so one sweep over
        // the candidates sorted by start keeps exactly the ones holding the
        // query (Codex security review of PR #482: no full scan per line).
        let mut queries: Vec<(u64, u64, bool)> = distinct(&hunk.added_lines)
            .into_iter()
            .map(|l| (l, l, nonblank_lines.contains(&l)))
            .collect();
        let mut unattributed_lines = 0;
        for gap in distinct(&hunk.removal_gaps) {
            if gap > 1 {
                queries.push((gap - 1, gap, nonblank_gaps.contains(&gap)));
            } else if nonblank_gaps.contains(&gap) {
                unattributed_lines += 1;
            }
        }
        queries.sort_unstable();
        let mut by_start: Vec<usize> = (0..candidates.len()).collect();
        by_start.sort_unstable_by_key(|&i| candidates[i].start);
        let mut next = 0;
        let mut active: BTreeSet<(u64, usize)> = BTreeSet::new();
        let mut work = 0usize;
        let mut budget_exceeded = false;
        for (from, to, nonblank) in queries {
            while next < by_start.len() && candidates[by_start[next]].start <= from {
                let i = by_start[next];
                active.insert((candidates[i].end, i));
                next += 1;
            }
            while active.first().is_some_and(|&(end, _)| end < to) {
                active.pop_first();
            }
            work += active.len();
            if work > ATTRIBUTION_BUDGET {
                budget_exceeded = true;
                break;
            }
            let owners = innermost_of(&candidates, active.iter().map(|&(_, i)| i).collect());
            if owners.is_empty() && nonblank {
                unattributed_lines += 1;
            }
            for i in owners {
                candidates[i].overlap += 1;
            }
        }
        FileAttribution {
            file_known: self.known_files.contains(&hunk.file_path),
            candidates,
            unattributed_lines,
            budget_exceeded,
        }
    }
}

/// `term(x)` for each of `items`, joined with ` OR `.
fn or_of(items: &[&str], term: impl Fn(&str) -> String) -> String {
    items
        .iter()
        .map(|i| term(i))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// The longest of `files` that `id` extends with `::`.
fn owning_file<'f>(files: &[&'f str], id: &str) -> Option<&'f str> {
    files
        .iter()
        .copied()
        .filter(|f| {
            id.strip_prefix(*f)
                .is_some_and(|rest| rest.starts_with("::"))
        })
        .max_by_key(|f| f.len())
}

fn distinct(lines: &[u64]) -> Vec<u64> {
    let mut out = lines.to_vec();
    out.sort_unstable();
    out.dedup();
    out
}

/// Indices of the candidates holding `from..=to` that enclose no other
/// candidate holding it: a method wins over its class, also on one line
/// (`class A { m() {} }`), but siblings that neither range nor id can tell
/// apart (`fn a() {} fn b() {}` on one line) are all kept.
///
/// O(n log n) in the holding candidates (Codex security review of PR #482:
/// thousands of constants on one line made a pairwise check quadratic). All
/// of them hold `from..=to`, and `encloses` is strict containment or, on an
/// equal range, a deeper id. Strict containment is a sweep by start
/// (descending) and end (ascending); the deeper id on an equal range is a
/// prefix lookup among that range's sorted ids.
#[cfg(test)]
fn innermost(candidates: &[Candidate], from: u64, to: u64) -> Vec<usize> {
    let holding: Vec<usize> = (0..candidates.len())
        .filter(|&i| candidates[i].start <= from && to <= candidates[i].end)
        .collect();
    innermost_of(candidates, holding)
}

/// `innermost` over `holding`, the candidates that all hold one query.
fn innermost_of(candidates: &[Candidate], mut holding: Vec<usize>) -> Vec<usize> {
    holding.sort_unstable_by(|&a, &b| {
        let (a, b) = (&candidates[a], &candidates[b]);
        b.start.cmp(&a.start).then(a.end.cmp(&b.end))
    });
    let mut kept = Vec::new();
    // Smallest end among the ranges already swept: each starts at or after
    // the current one, so an end at or before the current end means a
    // strictly smaller range inside it.
    let mut min_end = u64::MAX;
    for group in holding.chunk_by(|&a, &b| {
        (candidates[a].start, candidates[a].end) == (candidates[b].start, candidates[b].end)
    }) {
        let end = candidates[group[0]].end;
        if min_end > end {
            let mut ids: Vec<&str> = group.iter().map(|&i| candidates[i].id.as_str()).collect();
            ids.sort_unstable();
            kept.extend(
                group
                    .iter()
                    .copied()
                    .filter(|&i| !has_deeper(&ids, &candidates[i].id)),
            );
        }
        min_end = min_end.min(end);
    }
    kept.sort_unstable();
    kept
}

/// Whether a sorted `ids` holds an id nested under `id` (`id::…`).
fn has_deeper(ids: &[&str], id: &str) -> bool {
    let child = format!("{id}::");
    let at = ids.partition_point(|x| *x < child.as_str());
    ids.get(at).is_some_and(|x| x.starts_with(&child))
}

#[cfg(test)]
fn encloses(outer: &Candidate, inner: &Candidate) -> bool {
    let holds = outer.start <= inner.start && inner.end <= outer.end;
    let wider = (outer.end - outer.start) > (inner.end - inner.start);
    let deeper = inner
        .id
        .strip_prefix(outer.id.as_str())
        .is_some_and(|rest| rest.starts_with("::"));
    holds && (wider || deeper)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, start: u64, end: u64) -> Candidate {
        let name = id.rsplit("::").next().unwrap_or(id);
        Candidate {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            label: "Function",
            start,
            end,
            overlap: 0,
        }
    }

    fn owners(candidates: &[Candidate], line: u64) -> Vec<&str> {
        innermost(candidates, line, line)
            .into_iter()
            .map(|i| candidates[i].name.as_str())
            .collect()
    }

    #[test]
    fn a_nested_symbol_wins_over_its_container() {
        let c = [
            candidate("s.ts::Shape", 1, 8),
            candidate("s.ts::Shape::area", 2, 4),
        ];
        assert_eq!(owners(&c, 3), vec!["area"]);
        assert_eq!(owners(&c, 6), vec!["Shape"]);
        assert!(owners(&c, 9).is_empty());
    }

    /// Codex review of PR #482: `fn a() {} fn b() {}` on one line.
    #[test]
    fn siblings_on_one_line_all_keep_it() {
        let c = [candidate("m.rs::a", 5, 5), candidate("m.rs::b", 5, 5)];
        assert_eq!(owners(&c, 5), vec!["a", "b"]);
    }

    /// Codex security review of PR #482: the sweep equals the pairwise
    /// definition, and many siblings on one line stay fast.
    #[test]
    fn the_sweep_matches_the_pairwise_definition() {
        let pairwise = |c: &[Candidate], from: u64, to: u64| -> Vec<usize> {
            let holding: Vec<usize> = (0..c.len())
                .filter(|&i| c[i].start <= from && to <= c[i].end)
                .collect();
            holding
                .iter()
                .copied()
                .filter(|&i| !holding.iter().any(|&j| encloses(&c[i], &c[j])))
                .collect()
        };
        // Overlapping, nested, equal and id-nested ranges around lines 1..12.
        let mut c = Vec::new();
        let mut seed: u64 = 7;
        for n in 0..300 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let start = 1 + (seed >> 33) % 10;
            let end = start + (seed >> 45) % 4;
            let parent = (seed >> 20) % 6;
            let id = if n % 3 == 0 {
                format!("f.rs::s{parent}")
            } else {
                format!("f.rs::s{parent}::m{n}")
            };
            c.push(candidate(&id, start, end));
        }
        for line in 0..15 {
            assert_eq!(
                innermost(&c, line, line),
                pairwise(&c, line, line),
                "line {line}"
            );
            assert_eq!(innermost(&c, line, line + 1), pairwise(&c, line, line + 1));
        }
        let many: Vec<Candidate> = (0..50_000)
            .map(|n| candidate(&format!("f.rs::C{n}"), 1, 1))
            .collect();
        let started = std::time::Instant::now();
        assert_eq!(innermost(&many, 1, 1).len(), 50_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    /// Claude review of PR #482: `class A { m() {} }` on one line.
    #[test]
    fn on_one_line_the_deeper_id_wins() {
        let c = [candidate("s.ts::A", 3, 3), candidate("s.ts::A::m", 3, 3)];
        assert_eq!(owners(&c, 3), vec!["m"]);
        // `A::mx` is not inside `A::m`.
        let c = [
            candidate("s.ts::A::m", 3, 3),
            candidate("s.ts::A::mx", 3, 3),
        ];
        assert_eq!(owners(&c, 3), vec!["m", "mx"]);
    }
}

#[cfg(test)]
mod gap_tests {
    use super::*;

    fn loaded(candidates: Vec<Candidate>) -> DiffSymbols {
        DiffSymbols {
            known_files: HashSet::from(["m.rs".to_string()]),
            nested_files: Vec::new(),
            by_file: HashMap::from([("m.rs".to_string(), candidates)]),
            query_failed: false,
        }
    }

    /// A hunk whose lines and gaps are all non-blank.
    fn hunk(added: &[u64], gaps: &[u64]) -> FileHunk {
        FileHunk {
            file_path: "m.rs".to_string(),
            changed_lines: Vec::new(),
            added_lines: added.to_vec(),
            removal_gaps: gaps.to_vec(),
            nonblank_lines: added.to_vec(),
            nonblank_gaps: gaps.to_vec(),
            malformed: false,
            binary: false,
            renamed: false,
            is_new: false,
            is_deleted: false,
        }
    }

    fn function(id: &str, start: u64, end: u64) -> Candidate {
        Candidate {
            id: id.to_string(),
            name: id.to_string(),
            qualified_name: id.to_string(),
            label: "Function",
            start,
            end,
            overlap: 0,
        }
    }

    /// Claude review of PR #482: deleting `fn foo` just above `fn bar` (the
    /// removal sits before line 5, where `bar` starts) is not a change of `bar`.
    #[test]
    fn a_removal_just_above_a_symbol_is_not_that_symbol() {
        let d = loaded(vec![function("m.rs::bar", 5, 8)]);
        let a = d.attribute(&hunk(&[], &[5]));
        assert_eq!(a.candidates[0].overlap, 0);
        assert_eq!(a.unattributed_lines, 1);
    }

    #[test]
    fn a_removal_inside_a_symbol_is_that_symbol() {
        let d = loaded(vec![function("m.rs::bar", 5, 8)]);
        let a = d.attribute(&hunk(&[], &[7]));
        assert_eq!(a.candidates[0].overlap, 1);
        assert_eq!(a.unattributed_lines, 0);
    }

    /// Codex review of PR #482 (P2 on 205b1db): `@@ -2 +1,0 @@` removes the
    /// first statement of a function on lines 1-3; the gap sits before line 2,
    /// inside the function.
    #[test]
    fn a_removal_of_a_first_statement_is_its_function() {
        let d = loaded(vec![function("m.rs::f", 1, 3)]);
        let a = d.attribute(&hunk(&[], &[2]));
        assert_eq!(a.candidates[0].overlap, 1);
        // A gap before line 1 lies above every symbol.
        let a = d.attribute(&hunk(&[], &[1]));
        assert_eq!(a.candidates[0].overlap, 0);
        assert_eq!(a.unattributed_lines, 1);
    }

    /// Codex review of PR #482: a blank line inside a function (a multiline
    /// string) is that function's change; outside every symbol it is no gap.
    #[test]
    fn a_blank_change_counts_inside_a_symbol_only() {
        let d = loaded(vec![function("m.rs::f", 1, 3)]);
        let mut h = hunk(&[2, 9], &[9]);
        h.nonblank_lines.clear();
        h.nonblank_gaps.clear();
        let a = d.attribute(&h);
        assert_eq!(a.candidates[0].overlap, 1);
        assert_eq!(a.unattributed_lines, 0, "blank line and blank gap at 9");
    }

    #[test]
    fn rows_go_to_the_file_their_id_extends() {
        assert_eq!(
            owning_file(&["a.rs", "src/a.rs"], "src/a.rs::f"),
            Some("src/a.rs")
        );
        assert_eq!(owning_file(&["a.rs"], "a.rsx::f"), None);
        // Longest match, whatever the order: a directory literally named
        // `d::e.rs` beside a file `d` (both legal on Unix).
        assert_eq!(
            owning_file(&["d", "d::e.rs"], "d::e.rs::f"),
            Some("d::e.rs")
        );
        assert_eq!(
            owning_file(&["d::e.rs", "d"], "d::e.rs::f"),
            Some("d::e.rs")
        );
    }

    /// Codex security review of PR #482: a whole-file edit of 60,000
    /// one-line symbols is one sweep, not a scan of every symbol per line.
    #[test]
    fn many_changed_lines_over_many_symbols_stay_fast() {
        let n = 60_000u64;
        let c: Vec<Candidate> = (1..=n)
            .map(|l| function(&format!("m.rs::T{l}"), l, l))
            .collect();
        let lines: Vec<u64> = (1..=n).collect();
        let started = std::time::Instant::now();
        let a = loaded(c).attribute(&hunk(&lines, &[]));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(!a.budget_exceeded);
        assert_eq!(a.unattributed_lines, 0);
        assert!(a.candidates.iter().all(|c| c.overlap == 1));
    }

    /// Deep nesting over many changed lines runs out of the work budget and
    /// says so, instead of running unbounded.
    #[test]
    fn deep_nesting_over_many_lines_hits_the_work_budget() {
        let depth = 3_000u64;
        let lines = 2_000u64;
        let c: Vec<Candidate> = (0..depth)
            .map(|d| function(&format!("m.rs::N{d}"), 1, lines + depth - d))
            .collect();
        let changed: Vec<u64> = (1..=lines).collect();
        let a = loaded(c).attribute(&hunk(&changed, &[]));
        assert!(a.budget_exceeded);
    }
}
