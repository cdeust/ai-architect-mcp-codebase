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
//   for both. Each line now goes to the innermost candidate whose range holds it.
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
/// the innermost one whose range holds it.
///
/// postcondition: every changed line counts in exactly one candidate's
/// `overlap` or in `unattributed_lines`; candidates are unique by node `id`.
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
        match innermost(&candidates, line) {
            Some(i) => candidates[i].overlap += 1,
            None => unattributed_lines += 1,
        }
    }
    FileAttribution {
        file_known: !file_ids.is_empty(),
        candidates,
        unattributed_lines,
    }
}

/// Index of the candidate with the narrowest range holding `line`; the first
/// read wins a tie.
fn innermost(candidates: &[Candidate], line: u64) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.start <= line && line <= c.end)
        .min_by_key(|(_, c)| c.end - c.start)
        .map(|(i, _)| i)
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
