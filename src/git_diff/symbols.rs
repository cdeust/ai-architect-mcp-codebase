// git_diff::symbols — attributes a file's changed lines to the graph symbols
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
//   relies on), so Methods are read by that prefix instead.
// - A changed line inside a method also lies inside its class, and was counted
//   for both. Each line now goes to the innermost candidates whose range holds
//   it; siblings whose line ranges cannot be told apart all keep it.
//
// Lines that fall in no candidate are counted, so the caller can refuse to call
// a zero-symbol answer `exact`.

use std::collections::HashSet;

use crate::graph_store::{cypher_str, GraphStore};

/// Labels read through their `Defines_File_<label>` edge.
const DEFINED_LABELS: &[&str] = &["Function", "Struct", "Enum", "Trait"];

/// One symbol of the file with its line range.
pub(super) struct Candidate {
    pub(super) name: String,
    pub(super) qualified_name: String,
    pub(super) label: &'static str,
    pub(super) start: u64,
    pub(super) end: u64,
    /// Changed lines attributed to this candidate.
    pub(super) overlap: u64,
}

/// The symbols of one file and how its changed lines distribute over them.
pub(super) struct FileAttribution {
    /// Whether the graph holds a `File` with this path.
    pub(super) file_known: bool,
    pub(super) candidates: Vec<Candidate>,
    /// Changed lines that lie in no candidate's range.
    pub(super) unattributed_lines: u64,
}

/// Reads the symbols of `file_path` and attributes each of `changed_lines` to
/// the innermost ones whose range holds it (`innermost`).
///
/// postcondition: every changed line counts in `unattributed_lines` or in the
/// `overlap` of at least one candidate, never in a candidate that strictly
/// contains another holding it; candidates are unique by node `id`.
pub(super) fn attribute_lines(
    store: &GraphStore,
    file_path: &str,
    changed_lines: &[u64],
) -> FileAttribution {
    let file_ids = file_ids(store, file_path);
    let mut candidates = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let path_lit = cypher_str(file_path);
    for &label in DEFINED_LABELS {
        let cypher = format!(
            "MATCH (f:File)-[:Defines_File_{label}]->(n:{label}) \
             WHERE f.path = {path_lit} \
             RETURN n.id, n.name, n.qualified_name, n.start_line, n.end_line"
        );
        push_candidates(store, &cypher, label, &mut seen, &mut candidates);
    }
    for id in &file_ids {
        let prefix = cypher_str(&format!("{id}::"));
        let cypher = format!(
            "MATCH (n:Method) WHERE starts_with(n.id, {prefix}) \
             RETURN n.id, n.name, n.qualified_name, n.start_line, n.end_line"
        );
        push_candidates(store, &cypher, "Method", &mut seen, &mut candidates);
    }
    let mut unattributed_lines = 0;
    for &line in changed_lines {
        let owners = innermost(&candidates, line);
        if owners.is_empty() {
            unattributed_lines += 1;
        }
        for i in owners {
            candidates[i].overlap += 1;
        }
    }
    FileAttribution {
        file_known: !file_ids.is_empty(),
        candidates,
        unattributed_lines,
    }
}

/// Indices of the candidates holding `line` that hold no other candidate
/// holding it: a method wins over its class, but siblings whose line ranges
/// cannot be told apart (`fn a() {} fn b() {}` on one line) are all kept.
fn innermost(candidates: &[Candidate], line: u64) -> Vec<usize> {
    let holding: Vec<usize> = (0..candidates.len())
        .filter(|&i| candidates[i].start <= line && line <= candidates[i].end)
        .collect();
    holding
        .iter()
        .copied()
        .filter(|&i| {
            !holding
                .iter()
                .any(|&j| strictly_contains(&candidates[i], &candidates[j]))
        })
        .collect()
}

/// True when `outer`'s range holds `inner`'s and is wider.
fn strictly_contains(outer: &Candidate, inner: &Candidate) -> bool {
    outer.start <= inner.start
        && inner.end <= outer.end
        && (outer.end - outer.start) > (inner.end - inner.start)
}

fn file_ids(store: &GraphStore, file_path: &str) -> Vec<String> {
    let cypher = format!(
        "MATCH (f:File) WHERE f.path = {} RETURN f.id",
        cypher_str(file_path)
    );
    store
        .execute_query(&cypher)
        .map(|qr| {
            qr.rows
                .into_iter()
                .filter_map(|r| r.into_iter().next())
                .collect()
        })
        .unwrap_or_default()
}

fn push_candidates(
    store: &GraphStore,
    cypher: &str,
    label: &'static str,
    seen: &mut HashSet<String>,
    out: &mut Vec<Candidate>,
) {
    let Ok(qr) = store.execute_query(cypher) else {
        return;
    };
    for row in qr.rows {
        let Ok([id, name, qualified_name, start, end]) = <[String; 5]>::try_from(row) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let start: u64 = start.parse().unwrap_or(0);
        let end: u64 = end.parse().unwrap_or(0);
        out.push(Candidate {
            name,
            qualified_name,
            label,
            start,
            end: end.max(start),
            overlap: 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str, start: u64, end: u64) -> Candidate {
        Candidate {
            name: name.to_string(),
            qualified_name: name.to_string(),
            label: "Function",
            start,
            end,
            overlap: 0,
        }
    }

    fn owners(candidates: &[Candidate], line: u64) -> Vec<&str> {
        innermost(candidates, line)
            .into_iter()
            .map(|i| candidates[i].name.as_str())
            .collect()
    }

    #[test]
    fn a_nested_symbol_wins_over_its_container() {
        let c = [candidate("Shape", 1, 8), candidate("area", 2, 4)];
        assert_eq!(owners(&c, 3), vec!["area"]);
        assert_eq!(owners(&c, 6), vec!["Shape"]);
        assert!(owners(&c, 9).is_empty());
    }

    /// Codex review of PR #482: `fn a() {} fn b() {}` on one line.
    #[test]
    fn siblings_on_one_line_all_keep_it() {
        let c = [candidate("a", 5, 5), candidate("b", 5, 5)];
        assert_eq!(owners(&c, 5), vec!["a", "b"]);
    }
}
