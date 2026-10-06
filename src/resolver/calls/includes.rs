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

use crate::graph_store::GraphStore;

/// Extensions of the files whose `#include` directives the graph records as
/// `Import` nodes and that may be included in turn.
const C_FAMILY_EXTENSIONS: [&str; 12] = [
    "c", "h", "cc", "hh", "cpp", "hpp", "cxx", "c++", "hxx", "ipp", "inc", "inl",
];

/// The extensions of the files that are a translation unit of their own. Each one is
/// in `C_FAMILY_EXTENSIONS` and read by `Language::from_extension`: a source file
/// whose `#include` lines are not read would make `share_a_unit` false for the
/// files it includes.
pub(super) const SOURCE_EXTENSIONS: [&str; 5] = ["cpp", "cc", "cxx", "c++", "c"];

/// The files each C-family file includes, read once per resolution pass, and
/// the closure of that relation asked for on demand.
#[derive(Clone, Default)]
pub(super) struct IncludeGraph {
    direct: HashMap<String, Vec<String>>,
    /// The reverse of `direct`: the files that include each file directly.
    includers: HashMap<String, Vec<String>>,
    /// The files that may be in one translation unit with any file: a file with an
    /// `#include` the graph cannot read (`#include UNIT`) may include any file, so the files
    /// that reach one, and everything those include, are in it.
    in_opaque_unit: HashSet<String>,
    reached: RefCell<HashMap<String, HashSet<String>>>,
}

impl IncludeGraph {
    /// Reads every file id of the graph, then builds from `file_imports`
    /// (issue #404: which files each C-family file includes).
    pub(super) fn load(
        store: &GraphStore,
        file_imports: &HashMap<String, Vec<String>>,
    ) -> Result<Self, String> {
        let file_ids: HashSet<String> = store
            .execute_query("MATCH (f:File) RETURN f.id")?
            .rows
            .into_iter()
            .filter_map(|r| r.into_iter().next())
            .collect();
        Ok(Self::build(file_imports, &file_ids))
    }

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
        let includers = reverse(&direct);
        let opaque = file_imports
            .iter()
            .filter(|(file, paths)| is_c_family(file) && paths.iter().any(|p| is_computed(p)))
            .map(|(file, _)| file.as_str());
        let in_opaque_unit = walk(&direct, walk(&includers, opaque))
            .into_iter()
            .map(str::to_string)
            .collect();
        Self {
            direct,
            includers,
            in_opaque_unit,
            reached: RefCell::default(),
        }
    }

    /// True when `to` is `from` or a file `from` includes, directly or
    /// through other includes.
    pub(super) fn reaches(&self, from: &str, to: &str) -> bool {
        from == to || self.with_closure(from, |set| set.contains(to))
    }

    /// True when `a` and `b` can be compiled in one translation unit: a file is or
    /// includes both. This holds when either includes the other and when a third file
    /// includes both. A file that is in the unit of a computed include (`#include UNIT`)
    /// shares one with every file.
    pub(super) fn share_a_unit(&self, a: &str, b: &str) -> bool {
        self.in_opaque_unit.contains(a)
            || self.in_opaque_unit.contains(b)
            || !self.units_of(a).is_disjoint(&self.units_of(b))
    }

    /// The files whose translation unit holds `file`: itself and the files that include it.
    fn units_of<'a>(&'a self, file: &'a str) -> HashSet<&'a str> {
        walk(&self.includers, [file])
    }

    /// Runs `read` on the closure of `from`, computed once per pass.
    fn with_closure<R>(&self, from: &str, read: impl FnOnce(&HashSet<String>) -> R) -> R {
        if let Some(set) = self.reached.borrow().get(from) {
            return read(set);
        }
        let set = self.closure(from);
        let found = read(&set);
        self.reached.borrow_mut().insert(from.to_string(), set);
        found
    }

    fn closure(&self, from: &str) -> HashSet<String> {
        walk(&self.direct, [from])
            .into_iter()
            .map(str::to_string)
            .collect()
    }
}

/// The files `start` reaches along `edges`, `start` included.
fn walk<'a>(
    edges: &'a HashMap<String, Vec<String>>,
    start: impl IntoIterator<Item = &'a str>,
) -> HashSet<&'a str> {
    let mut pending: Vec<&str> = start.into_iter().collect();
    let mut seen: HashSet<&str> = pending.iter().copied().collect();
    while let Some(file) = pending.pop() {
        for next in edges.get(file).into_iter().flatten() {
            if seen.insert(next) {
                pending.push(next);
            }
        }
    }
    seen
}

fn reverse(direct: &HashMap<String, Vec<String>>) -> HashMap<String, Vec<String>> {
    let mut includers: HashMap<String, Vec<String>> = HashMap::new();
    for (file, targets) in direct {
        for target in targets {
            includers
                .entry(target.clone())
                .or_default()
                .push(file.clone());
        }
    }
    includers
}

/// An include path that is a macro name (`#include UNIT`): no directory, no extension,
/// upper case. A system header (`<vector>`) is lower case.
fn is_computed(path: &str) -> bool {
    path.len() > 1
        && path.chars().any(|c| c.is_ascii_uppercase())
        && path
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
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
    fn two_files_share_a_unit_when_one_includes_the_other_or_a_third_includes_both() {
        let g = graph(
            &[
                "d.cpp", "f.cpp", "g.cpp", "h.h", "x.cpp", "y.cpp", "lone.cpp",
            ],
            &[
                ("f.cpp", &["d.cpp"]),
                ("g.cpp", &["h.h"]),
                ("h.h", &["x.cpp", "y.cpp"]),
            ],
        );
        assert!(g.share_a_unit("d.cpp", "f.cpp") && g.share_a_unit("f.cpp", "d.cpp"));
        assert!(g.share_a_unit("x.cpp", "y.cpp") && g.share_a_unit("y.cpp", "x.cpp"));
        assert!(!g.share_a_unit("d.cpp", "x.cpp") && !g.share_a_unit("lone.cpp", "x.cpp"));
    }

    #[test]
    fn a_file_that_is_not_c_family_records_no_includes() {
        let g = graph(&["a.rs", "b.h"], &[("a.rs", &["b.h"])]);
        assert!(!g.reaches("a.rs", "b.h"));
    }
}
