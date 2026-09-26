// resolver::receiver::reexport: issue #373. An exact path (`crate::Set`,
// `fx::Set`, `crate::a::Set`) names the item `Set` of one module, and that item
// may itself be a `use`: `pub use task::Set;` at the root makes `crate::Set`
// the type of `task`, and `pub use ext::Set;` makes it a type of a crate the
// repository does not hold. So before an owner is compared with the path, the
// `use` declarations of the module the path ends in are read:
//
// - an explicit `use` binding the name replaces the path by the one it writes,
//   read from that module (a definition of the same name there would not
//   compile, so the `use` is what the name is);
// - otherwise the path stays (the module defines the name, or nothing does),
//   and each glob of the module adds the path the glob would give, since a
//   definition shadows a glob but the index cannot tell which one exists.
//
// A path to a crate outside the repository is read like a child module, which
// no owner has, so it admits nothing. Following stops on a path already seen.
// source: The Rust Reference, "Use declarations" (`use` paths are relative to
// the module, a glob is shadowed by an item of the same name) and "Paths".

use super::imports::ImportRow;
use super::written_path::{library_owns, path_segments, same_crate, Anchor, PathFacts};
use super::*;
use crate::graph_store::import_roots::CrateEvidence;

/// The paths `start` leads to once the `use` declarations of each module it
/// ends in are followed. A suffix anchor is kept as it is.
pub(super) fn follow(facts: &PathFacts, caller_file: &str, start: Anchor) -> Vec<Anchor> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut work = vec![start];
    while let Some(anchor) = work.pop() {
        // Terminates: a path leads further only through the `use` rows of a
        // module that has some, which are finitely many, so finitely many
        // paths are ever produced, and `seen` visits each one once.
        if !seen.insert(anchor.clone()) {
            continue;
        }
        let Some((module, name)) = anchor.exact_split() else {
            out.push(anchor);
            continue;
        };
        let rows = rows_of_module(facts, caller_file, &anchor, module);
        let (globs, explicit): (Vec<&ImportRow>, Vec<&ImportRow>) =
            rows.into_iter().partition(|r| r.is_glob);
        let explicit: Vec<&ImportRow> = explicit
            .into_iter()
            .filter(|r| r.binds() == name.as_str())
            .collect();
        let lib = anchor.library();
        if explicit.is_empty() {
            for glob in globs {
                let mut path = path_segments(&glob.path);
                path.push(name.clone());
                work.extend(anchor_in(facts.evidence, lib, module, &path));
            }
            out.push(anchor);
            continue;
        }
        for row in explicit {
            work.extend(anchor_in(
                facts.evidence,
                lib,
                module,
                &path_segments(&row.path),
            ));
        }
    }
    out
}

/// The exact anchor of `segments` written in `module` of the library `lib`
/// (`None`: the caller's crate). `None` when a `super` climbs past the root.
pub(super) fn anchor_in(
    evidence: &CrateEvidence,
    lib: Option<&str>,
    module: &[String],
    segments: &[String],
) -> Option<Anchor> {
    let within = |path: Vec<String>| match lib {
        Some(lib) => Anchor::Library {
            lib: lib.to_string(),
            segments: path,
        },
        None => Anchor::CrateRoot(path),
    };
    let first = segments.first()?.as_str();
    let joined = |base: &[String], rest: &[String]| [base, rest].concat();
    Some(match first {
        "crate" => within(segments[1..].to_vec()),
        "self" => within(joined(module, &segments[1..])),
        "super" => {
            let ups = segments.iter().take_while(|s| *s == "super").count();
            let kept = module.len().checked_sub(ups)?;
            within(joined(&module[..kept], &segments[ups..]))
        }
        _ if evidence.crate_names.contains(first) => Anchor::Library {
            lib: first.to_string(),
            segments: segments[1..].to_vec(),
        },
        _ => within(joined(module, segments)),
    })
}

/// The imports of every scope at `module` in the crate `anchor` is read in.
fn rows_of_module<'f>(
    facts: &'f PathFacts,
    caller_file: &str,
    anchor: &Anchor,
    module: &[String],
) -> Vec<&'f ImportRow> {
    facts
        .imports
        .scopes_at(module)
        .iter()
        .filter(|scope| {
            let file = extract_file_prefix_or_self(scope);
            match anchor.library() {
                Some(lib) => library_owns(facts.evidence, lib, &file),
                None => same_crate(facts.evidence, caller_file, &file),
            }
        })
        .flat_map(|scope| facts.imports.of_scope(scope))
        .collect()
}

#[cfg(test)]
#[path = "reexport_tests.rs"]
mod tests;
