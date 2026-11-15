// resolver::calls::cpp_classes: the C++ classes of the graph and the name lookup
// of a declared type (issues #406, #412).
//
// `CppClasses` reads every C++ class of the graph with the bases it names, the
// `using` aliases and `typedef`s, and the using-declarations and
// using-directives the sources write. `declared_family` answers which classes a
// receiver of a written type can have:
//
// - a type written without a qualifier is looked up as C++ looks it up (§6.4.1):
//   the innermost scope enclosing the caller that holds a class of exactly that
//   name — its own class, a base of it (a member type it inherits), then each
//   enclosing namespace, then the global scope — and that class only;
// - a name no enclosing scope holds, a name two bases of the caller's class
//   hold, a name written with a qualifier and a name an alias introduces are
//   read by path suffix (`family`), as before #412: a class whose path ends with
//   the name, on a `::` boundary, is of the family;
// - the lookup by scope is left for the path-suffix reading where a `using` the
//   caller sees, written in a scope at or inside the one that holds the class,
//   can make the name mean another class (`a_using_can_hide`): the graph holds
//   no edge from a `using` to its target.
//
// A `typedef` or `using` a source file writes is visible to a file that can be
// compiled with it in one translation unit (it includes it, it is included by
// it, or a third file includes both) and to any file when it is written in a
// header (`sees`).
//
// source: ISO/IEC 14882:2017 §6.4.1 (unqualified name lookup), §6.4.3.2 (a
// member of a base class is found in the derived class's scope), §10 (derived
// classes).

use std::collections::{HashMap, HashSet};

use super::includes::IncludeGraph;
use crate::graph_store::GraphStore;
use crate::language_provider::extract_file_prefix;
use crate::parser::generic_args::{split_outside_generics, strip_generic_groups};

/// The C++ classes of the graph: where each is, and the base classes it names.
#[derive(Default)]
pub(super) struct CppClasses {
    /// The path of a class (its qualified name without the file) to the raw
    /// names of its bases.
    bases: HashMap<String, Vec<String>>,
    /// The paths of the classes ending in a given last segment.
    by_last: HashMap<String, Vec<String>>,
    /// The `using` aliases and `typedef`s by last segment: what each names.
    aliases: HashMap<String, Vec<Alias>>,
    /// The using-declarations and using-directives the sources write.
    usings: Vec<Using>,
    /// Where each class is declared: its file and first line, one per node.
    declared_at: HashMap<String, Vec<(String, u64)>>,
    /// Which file includes which (a source file is seen by the files that include it).
    includes: IncludeGraph,
}

/// The call a declared type is looked up for: the caller's file, the scope the
/// caller is written in (its path without the file) and the line of the call.
pub(super) struct Caller<'a> {
    pub(super) file: &'a str,
    pub(super) scope: &'a str,
    pub(super) line: u64,
}

/// A `using X = T;` or `typedef T X;`: the path of `X`, the file that writes
/// it and the class path `T` names.
struct Alias {
    path: String,
    file: String,
    target: String,
}

/// True when a declaration written in `declared` is visible to a caller in
/// `from` by the file alone: a header may be included by any file, a source file is
/// a translation unit of its own and is seen by itself only.
fn unit_sees(from: &str, declared: &str) -> bool {
    declared == from
        || !matches!(
            declared.rsplit_once('.').map(|(_, ext)| ext),
            Some(ext) if super::includes::SOURCE_EXTENSIONS.contains(&ext)
        )
}

impl CppClasses {
    /// True when a declaration written in `declared` is visible to a caller in
    /// `from`: by the file alone (`unit_sees`), or because the two files can be one
    /// translation unit (`share_a_unit`): `from` includes `declared`, `declared`
    /// includes `from` (`using String = A;` then `#include "impl.h"`), or a third file
    /// includes both. The order of the includes is not read.
    fn sees(&self, from: &str, declared: &str) -> bool {
        unit_sees(from, declared) || self.includes.share_a_unit(declared, from)
    }

    /// Reads the `bases` of every C++ class (the `Struct` nodes of the
    /// language). A graph without the table, or without the column, yields none.
    pub(super) fn load(store: &GraphStore, includes: IncludeGraph) -> Self {
        let mut classes = Self {
            includes,
            ..Self::default()
        };
        let Ok(qr) =
            store.execute_query("MATCH (s:Struct) RETURN s.qualified_name, s.bases, s.language")
        else {
            return classes;
        };
        for row in qr.rows.iter().filter(|r| r.len() >= 3 && r[2] == "cpp") {
            let path = path_without_file(&row[0]).to_string();
            let bases = if row[1] == "Null(String)" {
                ""
            } else {
                &row[1]
            };
            let bases: Vec<String> = split_outside_generics(bases, ',')
                .into_iter()
                .map(class_path)
                .filter(|b| !b.is_empty())
                .collect();
            let last = path.rsplit("::").next().unwrap_or(&path).to_string();
            classes.by_last.entry(last).or_default().push(path.clone());
            classes.bases.entry(path).or_default().extend(bases);
        }
        classes.load_aliases(store);
        classes.load_usings(store);
        classes.load_positions(store);
        classes
    }

    /// Reads where each C++ class is declared. A graph without the column
    /// yields none, and the order of declarations is then not read.
    fn load_positions(&mut self, store: &GraphStore) {
        let Ok(qr) = store
            .execute_query("MATCH (s:Struct) RETURN s.qualified_name, s.start_line, s.language")
        else {
            return;
        };
        for row in qr.rows.iter().filter(|r| r.len() >= 3 && r[2] == "cpp") {
            let Ok(line) = row[1].parse::<u64>() else {
                continue;
            };
            let file = extract_file_prefix(&row[0]).unwrap_or_default();
            self.declared_at
                .entry(path_without_file(&row[0]).to_string())
                .or_default()
                .push((file, line));
        }
    }

    /// Reads what each C++ `using` alias (`TypeAlias.target_type`) and `typedef`
    /// (`Constant.type_annotation`) names. A constant that is no typedef is read
    /// as one too: a receiver is declared with a type name, which no constant of
    /// the same scope can bear.
    fn load_aliases(&mut self, store: &GraphStore) {
        for query in [
            "MATCH (a:TypeAlias) RETURN a.qualified_name, a.target_type, a.language",
            "MATCH (a:Constant) RETURN a.qualified_name, a.type_annotation, a.language",
        ] {
            let Ok(qr) = store.execute_query(query) else {
                continue;
            };
            for row in qr.rows.iter().filter(|r| r.len() >= 3 && r[2] == "cpp") {
                let target = class_path(&row[1]);
                if row[1] == "Null(String)" || target.is_empty() {
                    continue;
                }
                let path = path_without_file(&row[0]).to_string();
                let file = extract_file_prefix(&row[0]).unwrap_or_default();
                let last = path.rsplit("::").next().unwrap_or(&path).to_string();
                self.aliases
                    .entry(last)
                    .or_default()
                    .push(Alias { path, file, target });
            }
        }
    }

    /// The class named `name` (a path, possibly relative to a namespace), the
    /// classes an alias of that name stands for, and every base of them, through
    /// any depth. Of several aliases of one name, those the file `from` writes
    /// are the ones in scope; without any, every one is possible.
    pub(super) fn family(&self, name: &str, from: &str) -> Family {
        self.family_from(Seed::by_suffix(name), from)
    }

    /// The class of exactly `path` and its bases: unqualified lookup found that
    /// class, so a namesake of it in another namespace is not of the family.
    fn family_of_path(&self, path: &str, from: &str) -> Family {
        self.family_from(Seed::exact(path), from)
    }

    /// The family of one seed: its bases and what an alias of its name stands
    /// for, through any depth. A base and an alias target are written relative to
    /// a scope, so each is read by path suffix.
    fn family_from(&self, seed: Seed, from: &str) -> Family {
        let mut family = Family::default();
        let mut pending = vec![seed];
        while let Some(current) = pending.pop() {
            if current.name.is_empty() || !family.insert(&current) {
                continue;
            }
            let last = last_segment(&current.name);
            for path in self.by_last.get(last).into_iter().flatten() {
                if current.designates(path) {
                    pending.extend(
                        self.bases
                            .get(path)
                            .into_iter()
                            .flatten()
                            .map(|b| Seed::by_suffix(b)),
                    );
                }
            }
            pending.extend(
                self.alias_targets(last, &current, from)
                    .iter()
                    .map(|target| Seed::by_suffix(target)),
            );
        }
        family
    }

    fn alias_targets(&self, last: &str, name: &Seed, from: &str) -> Vec<String> {
        let named: Vec<&Alias> = self
            .aliases
            .get(last)
            .into_iter()
            .flatten()
            .filter(|a| name.designates(&a.path) && self.sees(from, &a.file))
            .collect();
        let local: Vec<&Alias> = named.iter().copied().filter(|a| a.file == from).collect();
        let chosen = if local.is_empty() { named } else { local };
        chosen.into_iter().map(|a| a.target.clone()).collect()
    }

    /// The classes a receiver declared of type `name` can have, for a caller
    /// written in the scope `scope` and in the file `from`: unqualified name
    /// lookup (§6.4.1) answers with the innermost enclosing scope of the caller
    /// that holds a class of exactly that name (`classes_named_in`). A name
    /// written with a qualifier, a name no enclosing scope holds, a name two
    /// bases of the caller's class hold and a name a `using` the caller sees can
    /// re-point (`a_using_can_hide`) keep the path-suffix reading.
    pub(super) fn declared_family(&self, name: &str, caller: &Caller) -> Family {
        let from = caller.file;
        if name.contains("::") {
            return self.family(name, from);
        }
        let mut inner: Vec<&str> = Vec::new();
        for enclosing in enclosing_scopes(caller.scope) {
            inner.push(enclosing);
            // An alias of that name in this scope is what C++ names; which
            // translation unit reaches it is `alias_targets`' reading.
            if self.an_alias_is_declared_in(enclosing, name, from) {
                break;
            }
            let Some(found) = self.classes_named_in(enclosing, name) else {
                break;
            };
            match found.as_slice() {
                // No scope holds the name here: the enclosing scope is next.
                [] => continue,
                // One class: the one C++ names, unless a `using` can re-point it
                // or a member class is declared after the call (a parameter's
                // type is read where the parameter is written).
                [path]
                    if !self.a_using_can_hide(name, from, &inner)
                        && !self.declared_after(path, enclosing, caller) =>
                {
                    return self.family_of_path(path, from)
                }
                // Two bases of the caller's class declare the name, or a `using`
                // can re-point it: the path-suffix reading answers.
                _ => break,
            }
        }
        self.family(name, from)
    }

    /// True when an alias (`using X = T`, `typedef`) of exactly `scope::name`
    /// is written in a file `from` sees.
    fn an_alias_is_declared_in(&self, scope: &str, name: &str, from: &str) -> bool {
        let path = qualify(scope, name);
        self.aliases
            .get(name)
            .into_iter()
            .flatten()
            .any(|a| a.path == path && self.sees(from, &a.file))
    }

    /// True when `path`, a member class of the class `scope`, is declared only
    /// in the caller's file and after the call: it is not in scope where a
    /// parameter's type is read (§6.4.1/7), so the class found cannot be trusted.
    fn declared_after(&self, path: &str, scope: &str, caller: &Caller) -> bool {
        if !self.holds_class_exactly(scope) {
            return false;
        }
        let positions = self.declared_at.get(path).map_or(&[][..], Vec::as_slice);
        !positions.is_empty()
            && positions
                .iter()
                .all(|(file, line)| file == caller.file && *line > caller.line)
    }

    /// The classes of exactly `scope::name`, and, when `scope` is a class, of
    /// `base::name` for every base of it: where C++ finds a member type it
    /// inherits. An alias (`using`, `typedef`) is not read here: which
    /// translation unit reaches which alias is the reading of `alias_targets`,
    /// which the path-suffix reading keeps.
    fn classes_named_in(&self, scope: &str, name: &str) -> Option<Vec<String>> {
        let mut holders = vec![scope.to_string()];
        holders.extend(
            self.class_and_bases(scope)?
                .into_iter()
                .filter(|base| base != scope),
        );
        let mut found: Vec<String> = holders
            .iter()
            .map(|holder| qualify(holder, name))
            .filter(|path| self.holds_class_exactly(path))
            .collect();
        found.sort_unstable();
        found.dedup();
        Some(found)
    }

    /// True when the graph holds a class of exactly `path`.
    fn holds_class_exactly(&self, path: &str) -> bool {
        self.by_last
            .get(last_segment(path))
            .is_some_and(|paths| paths.iter().any(|p| p == path))
    }

    /// The paths of the class `path` and of every class a base of it names,
    /// through any depth. A base is written relative to a scope, so it is read
    /// by path suffix; a base of no class of the graph is dropped. Empty when
    /// `path` is no class (a namespace, the global scope); `None` when a base
    /// names several classes, which one C++ names being unknown here.
    fn class_and_bases(&self, path: &str) -> Option<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut kept: HashSet<String> = HashSet::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending = vec![Seed::exact(path)];
        while let Some(current) = pending.pop() {
            // A seed is expanded once: a cycle or many namesakes naming one base
            // stays linear in the classes of that name (review 1 of #542).
            if !seen.insert(format!("{}{}", u8::from(current.exact), current.name)) {
                continue;
            }
            let named: Vec<&String> = self
                .by_last
                .get(last_segment(&current.name))
                .into_iter()
                .flatten()
                .filter(|class| current.designates(class))
                .collect();
            if named.len() > 1 {
                return None;
            }
            for class in named {
                if kept.insert(class.clone()) {
                    out.push(class.clone());
                    pending.extend(
                        self.bases
                            .get(class)
                            .into_iter()
                            .flatten()
                            .map(|base| Seed::by_suffix(base)),
                    );
                }
            }
        }
        Some(out)
    }
}

/// A name to look a class up by: a path matched exactly, or a name as the source
/// writes it, matched by path suffix.
struct Seed {
    name: String,
    exact: bool,
}

impl Seed {
    fn by_suffix(name: &str) -> Self {
        Self {
            name: name.to_string(),
            exact: false,
        }
    }

    fn exact(path: &str) -> Self {
        Self {
            name: path.to_string(),
            exact: true,
        }
    }

    /// True when the class of full path `path` is the one this seed names.
    fn designates(&self, path: &str) -> bool {
        if self.exact {
            path == self.name
        } else {
            names_class(path, &self.name)
        }
    }
}

/// A set of class names: full paths unqualified lookup resolved, and names as
/// the source writes them, which a class whose path ends with one bears.
#[derive(Default)]
pub(super) struct Family {
    by_suffix: HashSet<String>,
    exact: HashSet<String>,
}

impl Family {
    /// Adds a seed; false when it was already there.
    fn insert(&mut self, seed: &Seed) -> bool {
        let set = if seed.exact {
            &mut self.exact
        } else {
            &mut self.by_suffix
        };
        set.insert(seed.name.clone())
    }

    /// True when `class`, a full path, is one of the names.
    pub(super) fn holds(&self, class: &str) -> bool {
        self.exact.contains(class) || self.by_suffix.iter().any(|name| names_class(class, name))
    }
}

/// The last segment of a path: `Box` of `a::Box`.
fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// `name` in the scope `scope`: `a::b::Box` of `Box` in `a::b`, `Box` at global
/// scope (an empty `scope`).
fn qualify(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_string()
    } else {
        format!("{scope}::{name}")
    }
}

/// The scopes a name written in `scope` is looked up in, the innermost first:
/// `a::b`, `a`, then the global scope (empty).
fn enclosing_scopes(scope: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut current = scope;
    while !current.is_empty() {
        out.push(current);
        current = current.rsplit_once("::").map_or("", |(outer, _)| outer);
    }
    out.push("");
    out
}

/// True when `name`, written as a path or relative to a namespace, designates
/// the class of full path `path`.
pub(super) fn names_class(path: &str, name: &str) -> bool {
    path == name || path.strip_suffix(name).is_some_and(|p| p.ends_with("::"))
}

/// `Base` of `public Base<T>`, `::ns::Base` or `ns::Base`: a class as a path,
/// without access specifier, `virtual`, generic arguments or leading `::`.
fn class_path(raw: &str) -> String {
    let plain = strip_generic_groups(raw);
    let name: String = plain
        .split_whitespace()
        .skip_while(|t| {
            matches!(
                *t,
                "public" | "protected" | "private" | "virtual" | "typename"
            )
        })
        .collect();
    name.trim_start_matches("::").to_string()
}

/// A qualified name without its file: `etl::Bloom::width#1` of
/// `b.cpp::etl::Bloom::width#1`.
pub(super) fn path_without_file(qn: &str) -> &str {
    match extract_file_prefix(qn) {
        Some(file) => qn[file.len()..].trim_start_matches("::"),
        None => qn,
    }
}

#[path = "cpp_classes_usings.rs"]
mod usings;
#[cfg(test)]
use usings::using_of;
use usings::Using;

#[cfg(test)]
#[path = "cpp_classes_tests.rs"]
mod tests;
