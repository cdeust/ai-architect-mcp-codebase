// resolver::calls::includes: which files a file's `#include` directives copy
// into it (issue #404).
//
// A `static` function has internal linkage: it is named by the translation
// unit that holds its definition, and a translation unit is a file plus every
// file its `#include` directives paste in, directly or through another
// include, whatever the extension of the pasted file (a unity build includes
// `.c` files). The graph records each directive as an `Import` whose path is
// the text between the delimiters; this module resolves that text to the
// files of the repository and follows the chain.
//
// A path is resolved as the preprocessor's quoted form does: first against the
// directory of the including file, then against the include directories, which
// the graph does not know. There the path is matched as a trailing part of the
// file ids, and every file it matches counts as included: the build picks
// one, and the graph does not know which, so reachability is over-approximated
// there rather than guessed.
//
// source: ISO/IEC 9899:2018 §5.1.1.1 (translation units), §6.2.2p3 (internal
// linkage), §6.10.2 (source file inclusion).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// Extensions of the files whose `#include` directives the graph records as
/// `Import` nodes and that may be included in turn.
const C_FAMILY_EXTENSIONS: [&str; 10] = [
    "c", "h", "cc", "hh", "cpp", "hpp", "cxx", "hxx", "inc", "inl",
];

/// The files each C-family file includes, read once per resolution pass, and
/// the closure of that relation asked for on demand.
pub(super) struct IncludeGraph {
    direct: HashMap<String, Vec<String>>,
    reached: RefCell<HashMap<String, HashSet<String>>>,
}

impl IncludeGraph {
    /// `file_imports` maps a file id to the import paths written in it;
    /// `file_ids` is every file of the graph.
    pub(super) fn build(
        file_imports: &HashMap<String, Vec<String>>,
        file_ids: &HashSet<String>,
    ) -> Self {
        let by_basename = by_basename(file_ids);
        let direct = file_imports
            .iter()
            .filter(|(file, _)| is_c_family(file))
            .map(|(file, paths)| {
                let mut targets: Vec<String> = paths
                    .iter()
                    .flat_map(|p| resolve_include(file, p, file_ids, &by_basename))
                    .filter(|t| t != file)
                    .collect();
                targets.sort_unstable();
                targets.dedup();
                (file.clone(), targets)
            })
            .collect();
        Self {
            direct,
            reached: RefCell::default(),
        }
    }

    /// True when `to` is `from` or a file `from` includes, directly or
    /// through other includes.
    pub(super) fn reaches(&self, from: &str, to: &str) -> bool {
        if from == to {
            return true;
        }
        if let Some(set) = self.reached.borrow().get(from) {
            return set.contains(to);
        }
        let set = self.closure(from);
        let found = set.contains(to);
        self.reached.borrow_mut().insert(from.to_string(), set);
        found
    }

    fn closure(&self, from: &str) -> HashSet<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending: Vec<&str> = vec![from];
        while let Some(file) = pending.pop() {
            for next in self.direct.get(file).into_iter().flatten() {
                if seen.insert(next.clone()) {
                    pending.push(next);
                }
            }
        }
        seen
    }
}

fn is_c_family(file: &str) -> bool {
    file.rsplit_once('.')
        .is_some_and(|(_, ext)| C_FAMILY_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

fn by_basename(file_ids: &HashSet<String>) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in file_ids {
        map.entry(id.rsplit('/').next().unwrap_or(id))
            .or_default()
            .push(id);
    }
    map
}

/// The files the include `path` written in `includer` can name.
fn resolve_include(
    includer: &str,
    path: &str,
    file_ids: &HashSet<String>,
    by_basename: &HashMap<&str, Vec<&str>>,
) -> Vec<String> {
    let dir = includer.rsplit_once('/').map_or("", |(dir, _)| dir);
    if let Some(relative) = normalize(dir, path) {
        if file_ids.contains(&relative) {
            return vec![relative];
        }
    }
    // The include directories are unknown: keep the path without its leading
    // `./` and `../` and match it as a trailing part of the file ids.
    let tail = path.trim_start_matches("./");
    let tail = tail.trim_start_matches("../");
    let Some(candidates) = by_basename.get(tail.rsplit('/').next().unwrap_or(tail)) else {
        return Vec::new();
    };
    candidates
        .iter()
        .filter(|id| **id == tail || id.ends_with(&format!("/{tail}")))
        .map(|id| (*id).to_string())
        .collect()
}

/// `dir/path` with `.` and `..` segments folded, or `None` when `..` leaves
/// the repository.
fn normalize(dir: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(files: &[&str], imports: &[(&str, &[&str])]) -> IncludeGraph {
        let ids: HashSet<String> = files.iter().map(|f| (*f).to_string()).collect();
        let map = imports
            .iter()
            .map(|(f, ps)| {
                (
                    (*f).to_string(),
                    ps.iter().map(|p| (*p).to_string()).collect(),
                )
            })
            .collect();
        IncludeGraph::build(&map, &ids)
    }

    #[test]
    fn a_path_is_first_read_against_the_directory_of_the_includer() {
        let g = graph(&["a/x.c", "a/x.h", "b/x.h"], &[("a/x.c", &["x.h"])]);
        assert!(g.reaches("a/x.c", "a/x.h"));
        assert!(!g.reaches("a/x.c", "b/x.h"));
    }

    #[test]
    fn a_path_missing_next_to_the_includer_matches_the_tail_of_every_file_id() {
        let g = graph(
            &["src/u.c", "inc/util/c.h", "port/util/c.h", "port/util/d.h"],
            &[("src/u.c", &["util/c.h"])],
        );
        assert!(g.reaches("src/u.c", "inc/util/c.h"));
        assert!(g.reaches("src/u.c", "port/util/c.h"));
        assert!(!g.reaches("src/u.c", "port/util/d.h"));
    }

    #[test]
    fn a_tail_never_matches_inside_a_directory_name() {
        let g = graph(&["u.c", "myutil/c.h"], &[("u.c", &["util/c.h"])]);
        assert!(!g.reaches("u.c", "myutil/c.h"));
    }

    #[test]
    fn the_closure_follows_chains_and_survives_cycles() {
        let g = graph(
            &["t.c", "m.h", "l.h"],
            &[("t.c", &["m.h"]), ("m.h", &["l.h"]), ("l.h", &["m.h"])],
        );
        assert!(g.reaches("t.c", "l.h"));
        assert!(!g.reaches("l.h", "t.c"));
    }

    #[test]
    fn a_file_that_is_not_c_family_records_no_includes() {
        let g = graph(&["a.rs", "b.h"], &[("a.rs", &["b.h"])]);
        assert!(!g.reaches("a.rs", "b.h"));
    }
}
