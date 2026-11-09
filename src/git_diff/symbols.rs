// git_diff::symbols — attributes a diff's changed lines to the graph symbols
// that contain them (issue #446).
//
// Two defects made `detect_changes` report a modified method as its class, or as
// nothing:
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
// - A changed line inside a method also lies inside its class, and was counted
//   for both. Each line now goes to the innermost candidates whose range holds
//   it; on equal ranges the deeper id (`A::m` over `A`) wins, and siblings that
//   neither range nor id can tell apart all keep it.
// - A pure removal was credited to the line after it: deleting `fn foo` just
//   above `fn bar` reported `bar`. It now counts for a symbol only when that
//   symbol spans the lines on both sides of it.
//
// Every label is read once per diff (one `starts_with` query per label and
// chunk of files), not once per file.
//
// Lines that fall in no candidate, and queries that fail, are reported, so the
// caller can refuse to call an incomplete answer `exact`.

use std::collections::{HashMap, HashSet};

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

/// The symbols of every file a diff names, read once per diff.
pub(super) struct DiffSymbols {
    /// `File.id`s the graph holds among the diff's paths (`File.id` is the
    /// path: `indexer::persist::insert_file_node`).
    known_files: HashSet<String>,
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
}

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
        let cond = or_of(chunk, |p| format!("f.id = {}", cypher_str(p)));
        match store.execute_query(&format!("MATCH (f:File) WHERE {cond} RETURN f.id")) {
            Ok(qr) => self
                .known_files
                .extend(qr.rows.into_iter().filter_map(|r| r.into_iter().next())),
            Err(_) => self.query_failed = true,
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

    /// Attributes `lines` (lines present in the new file) and `removal_gaps`
    /// (a pure removal just above that line) of `file_path` to its symbols.
    ///
    /// postcondition: every distinct line and gap counts in
    /// `unattributed_lines` or in the `overlap` of at least one candidate,
    /// never in a candidate that encloses another holding it.
    pub(super) fn attribute(
        &self,
        file_path: &str,
        lines: &[u64],
        removal_gaps: &[u64],
    ) -> FileAttribution {
        let mut candidates = self.by_file.get(file_path).cloned().unwrap_or_default();
        let mut unattributed_lines = 0;
        for line in distinct(lines) {
            let owners = innermost(&candidates, line, line);
            unattributed_lines += u64::from(owners.is_empty());
            for i in owners {
                candidates[i].overlap += 1;
            }
        }
        // A removal between lines p-1 and p belongs to a symbol only when that
        // symbol spans both: deleting `fn foo` just above `fn bar` is not a
        // change of `bar`.
        for gap in distinct(removal_gaps) {
            let owners = innermost(&candidates, gap.saturating_sub(1).max(1), gap);
            let inside = gap > 1 && !owners.is_empty();
            unattributed_lines += u64::from(!inside);
            if inside {
                for i in owners {
                    candidates[i].overlap += 1;
                }
            }
        }
        FileAttribution {
            file_known: self.known_files.contains(file_path),
            candidates,
            unattributed_lines,
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
fn innermost(candidates: &[Candidate], from: u64, to: u64) -> Vec<usize> {
    let holding: Vec<usize> = (0..candidates.len())
        .filter(|&i| candidates[i].start <= from && to <= candidates[i].end)
        .collect();
    holding
        .iter()
        .copied()
        .filter(|&i| {
            !holding
                .iter()
                .any(|&j| encloses(&candidates[i], &candidates[j]))
        })
        .collect()
}

/// True when `outer`'s range holds `inner`'s and is wider, or the ranges are
/// equal and `inner`'s id extends `outer`'s (`A::m` inside `A`).
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
            by_file: HashMap::from([("m.rs".to_string(), candidates)]),
            query_failed: false,
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
        let a = d.attribute("m.rs", &[], &[5]);
        assert_eq!(a.candidates[0].overlap, 0);
        assert_eq!(a.unattributed_lines, 1);
    }

    #[test]
    fn a_removal_inside_a_symbol_is_that_symbol() {
        let d = loaded(vec![function("m.rs::bar", 5, 8)]);
        let a = d.attribute("m.rs", &[], &[7]);
        assert_eq!(a.candidates[0].overlap, 1);
        assert_eq!(a.unattributed_lines, 0);
    }

    /// Codex review of PR #482 (P2 on 205b1db): `@@ -2 +1,0 @@` removes the
    /// first statement of a function on lines 1-3; the gap sits before line 2,
    /// inside the function.
    #[test]
    fn a_removal_of_a_first_statement_is_its_function() {
        let d = loaded(vec![function("m.rs::f", 1, 3)]);
        let a = d.attribute("m.rs", &[], &[2]);
        assert_eq!(a.candidates[0].overlap, 1);
        // A gap before line 1 lies above every symbol.
        let a = d.attribute("m.rs", &[], &[1]);
        assert_eq!(a.candidates[0].overlap, 0);
        assert_eq!(a.unattributed_lines, 1);
    }

    #[test]
    fn rows_go_to_the_file_their_id_extends() {
        assert_eq!(
            owning_file(&["a.rs", "src/a.rs"], "src/a.rs::f"),
            Some("src/a.rs")
        );
        assert_eq!(owning_file(&["a.rs"], "a.rsx::f"), None);
    }
}
