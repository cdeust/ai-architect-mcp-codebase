// resolver::receiver::binding: issues #373 and #380. What a one-segment type
// name (`Set`) of the caller's module refers to, read in the order Rust gives
// names of a module, from the caller's module (its file, then the inline
// modules around it) outwards:
//
// 1. a type (struct, enum, union, alias) of that name defined in the module:
//    the name is that type; in the caller's own module the lookup by name
//    already prefers it, so it keeps that lookup (`Binding::ByName`);
// 2. exactly one explicit `use` binding the name: the path it writes;
// 3. two explicit `use`s with distinct paths (two `#[cfg]` arms): in doubt,
//    the call declines;
// 4. only `use super::*` globs: the same rules, one module up;
// 5. other globs: each glob's `prefix::Name`; the one prefix that admits a
//    candidate names it, and two or more decline (Rust rejects a name two
//    globs give, so the index is missing a gate); when none does, a glob of a
//    crate outside the repository may give the name, so the call declines;
// 6. nothing (no binding, or only globs of repository modules that lack the
//    name): the lookup by name.
//
// source: The Rust Reference, "Use declarations" (an item or explicit `use`
// shadows a glob; a name two globs give is an error only when used) and
// "Paths" (`super`).

use super::imports::{caller_scope, scope_module_path, ImportRow};
use super::reexport::anchor_in;
use super::written_path::{
    library_owns, module_path_of, path_segments, same_crate, Anchor, PathFacts, WrittenPath,
};
use super::*;

/// What a one-segment type name of the caller's module refers to.
pub(in crate::resolver) enum Binding<'e> {
    /// Rules 1 (in the caller's module) and 6: the lookup by name.
    ByName,
    /// The name is read as `hint`, whose owners `path` admits.
    Path {
        hint: String,
        path: WrittenPath<'e>,
        /// A cargo-less tree whose path starts with a name that may be this
        /// very crate: when no owner matches, the lookup by name applies.
        fallback: bool,
    },
    /// Rule 3, a `super` past the crate root, or a path that cannot be read.
    Decline,
}

/// One module the rules are read in: its path from the crate root and the
/// scopes (a file, or an inline `mod`) that declare its `use`s.
struct Level {
    module: Vec<String>,
    scopes: Vec<String>,
}

/// The binding of `name` for a call in `caller_qn`. `admits_any(hint, path)`
/// says whether the call has a candidate `path` admits (rule 5).
pub(in crate::resolver) fn bind<'e>(
    facts: &PathFacts<'e>,
    caller_qn: &str,
    name: &str,
    admits_any: &dyn Fn(&str, &WrittenPath<'e>) -> bool,
) -> Binding<'e> {
    let caller_file = extract_file_prefix_or_self(caller_qn);
    let scope = caller_scope(facts.idx, caller_qn);
    if defined_in_scope(facts.idx, &scope, name) {
        return Binding::ByName;
    }
    let mut level = Level {
        module: scope_module_path(facts.evidence, &scope),
        scopes: vec![scope],
    };
    let mut top = true;
    loop {
        if !top && defined_at(facts, &caller_file, &level.module, name) {
            let mut written = level.module.clone();
            written.push(name.to_string());
            let path = WrittenPath::exact(facts, &caller_file, Anchor::CrateRoot(written));
            return Binding::Path {
                hint: name.to_string(),
                path,
                fallback: false,
            };
        }
        let rows: Vec<&ImportRow> = level
            .scopes
            .iter()
            .flat_map(|s| facts.imports.of_scope(s))
            .collect();
        let explicit = distinct(rows.iter().filter(|r| !r.is_glob && r.binds() == name));
        match explicit.as_slice() {
            [] => {}
            [path] => return read(facts, &caller_file, &level.module, path.clone()),
            _ => return Binding::Decline,
        }
        let globs = distinct(rows.iter().filter(|r| r.is_glob));
        if globs.is_empty() {
            return Binding::ByName;
        }
        if globs.iter().all(|g| g == "super") {
            let Some(parent) = up(facts, &caller_file, &level) else {
                return Binding::Decline;
            };
            level = parent;
            top = false;
            continue;
        }
        return through_globs(facts, &caller_file, &level.module, name, &globs, admits_any);
    }
}

/// Rule 5: the one glob of `globs` whose `prefix::name` admits a candidate.
/// When none does, the lookup by name applies only if every glob is a module
/// of the repository (which then lacks the name); a glob of a crate outside
/// the repository may give it, so the call declines. Without Cargo facts no
/// crate is known, and the lookup by name stays (#357).
fn through_globs<'e>(
    facts: &PathFacts<'e>,
    caller_file: &str,
    module: &[String],
    name: &str,
    globs: &[String],
    admits_any: &dyn Fn(&str, &WrittenPath<'e>) -> bool,
) -> Binding<'e> {
    let mut hits = globs.iter().filter_map(|glob| {
        let written = format!("{glob}::{name}");
        let path = WrittenPath::imported(facts, caller_file, module, &written)?;
        admits_any(&written, &path).then_some((written, path))
    });
    match (hits.next(), hits.next()) {
        (None, _) => {
            let known = |glob: &String| in_repository(facts, caller_file, module, glob);
            if !facts.evidence.known || globs.iter().all(known) {
                Binding::ByName
            } else {
                Binding::Decline
            }
        }
        (Some((hint, path)), None) => Binding::Path {
            fallback: may_be_this_crate(facts, &hint),
            hint,
            path,
        },
        (Some(_), Some(_)) => Binding::Decline,
    }
}

/// True when the glob `glob` of `module` reads a module (a file, an inline
/// `mod`, an enum) of the repository: of the caller's crate, or of a library
/// of the repository.
fn in_repository(facts: &PathFacts, caller_file: &str, module: &[String], glob: &str) -> bool {
    let Some(anchor) = anchor_in(facts.evidence, None, module, &path_segments(glob)) else {
        return false;
    };
    match &anchor {
        Anchor::CrateRoot(path) => facts
            .imports
            .module_files(path)
            .iter()
            .any(|f| same_crate(facts.evidence, caller_file, f)),
        Anchor::Library { lib, segments } => facts
            .imports
            .module_files(segments)
            .iter()
            .any(|f| library_owns(facts.evidence, lib, f)),
        Anchor::Suffix(_) => false,
    }
}

/// Rule 2: the path an explicit `use` of `module` writes.
fn read<'e>(
    facts: &PathFacts<'e>,
    caller_file: &str,
    module: &[String],
    hint: String,
) -> Binding<'e> {
    match WrittenPath::imported(facts, caller_file, module, &hint) {
        Some(path) => Binding::Path {
            fallback: may_be_this_crate(facts, &hint),
            hint,
            path,
        },
        None => Binding::Decline,
    }
}

/// Without Cargo facts, a path that starts with neither `crate`, `self` nor
/// `super` may name this very crate by its package name.
fn may_be_this_crate(facts: &PathFacts, path: &str) -> bool {
    let first = path.split("::").next().unwrap_or("");
    !facts.evidence.known && !["crate", "self", "super"].contains(&first)
}

/// The module above `level`: the enclosing inline `mod` of the same file, or
/// the parent module of a file, read in every scope of the caller's crate at
/// that path. `None` above the crate root.
fn up(facts: &PathFacts, caller_file: &str, level: &Level) -> Option<Level> {
    let (_, parent) = level.module.split_last()?;
    let inline_parent = match level.scopes.as_slice() {
        [scope] => scope
            .rsplit_once("::")
            .filter(|(p, _)| p.len() >= extract_file_prefix_or_self(scope).len())
            .map(|(p, _)| p.to_string()),
        _ => None,
    };
    let scopes = match inline_parent {
        Some(scope) => vec![scope],
        None => facts
            .imports
            .scopes_at(parent)
            .iter()
            .filter(|s| same_crate(facts.evidence, caller_file, &extract_file_prefix_or_self(s)))
            .cloned()
            .collect(),
    };
    Some(Level {
        module: parent.to_vec(),
        scopes,
    })
}

/// Labels of the items that make a type name.
const TYPE_LABELS: [&str; 4] = ["Struct", "Enum", "Union", "TypeAlias"];

/// True when `scope` itself defines a type named `name`.
fn defined_in_scope(idx: &SymbolIndex, scope: &str, name: &str) -> bool {
    idx.by_name.get(name).is_some_and(|entries| {
        entries.iter().any(|e| {
            TYPE_LABELS.contains(&e.label.as_str())
                && e.qualified_name.rsplit_once("::").map(|(p, _)| p) == Some(scope)
        })
    })
}

/// True when the module `module` of the caller's crate defines a type `name`.
fn defined_at(facts: &PathFacts, caller_file: &str, module: &[String], name: &str) -> bool {
    facts.idx.by_name.get(name).is_some_and(|entries| {
        entries.iter().any(|e| {
            let file = extract_file_prefix_or_self(&e.qualified_name);
            let path = module_path_of(facts.evidence, &e.qualified_name);
            TYPE_LABELS.contains(&e.label.as_str())
                && path.split_last().is_some_and(|(_, m)| m == module)
                && same_crate(facts.evidence, caller_file, &file)
        })
    })
}

/// The distinct paths of `rows`, sorted.
fn distinct<'r>(rows: impl Iterator<Item = &'r &'r ImportRow>) -> Vec<String> {
    let mut paths: Vec<String> = rows.map(|r| r.path.clone()).collect();
    paths.sort();
    paths.dedup();
    paths
}
