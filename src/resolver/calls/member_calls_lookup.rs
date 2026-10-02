// resolver::calls::member_calls::lookup: which declaration a type written in a C++
// receiver names, seen from the caller's file and scope (issue #412).

use super::*;

/// Where a call is written: its file and the namespaces and classes around it,
/// outermost first (the owner path of its qualified name; empty for a function of
/// the global namespace), and whether the innermost of them is the class of the
/// method that makes the call.
pub(super) struct Caller<'a> {
    pub(super) file: &'a str,
    pub(super) scope: Vec<String>,
    pub(super) in_class: bool,
}

/// True when a declaration written in `declared` is visible to a caller in
/// `from`: a header may be included by any file, a source file is a translation
/// unit of its own and is seen by itself only.
pub(super) fn sees(from: &str, declared: &str) -> bool {
    declared == from
        || !matches!(
            declared.rsplit_once('.').map(|(_, ext)| ext),
            Some("cpp" | "cc" | "cxx" | "c++" | "cp" | "c" | "C")
        )
}

impl CppClasses {
    /// The class a type written `name` designates for a caller in `from` inside
    /// `scope` (the namespaces and classes around it, outermost first), and what
    /// `family` adds. C++ looks a name up from the innermost enclosing scope
    /// outwards, so `timer_data` in `etl::icallback_timer::start` is
    /// `etl::icallback_timer::timer_data`, not every class of the repository whose
    /// path ends in `timer_data`. In order:
    /// 1. the declaration the innermost scope holds, when it is a typedef or
    ///    `using` of the caller's own file (nothing another file writes competes);
    /// 2. a typedef or `using` of the caller's own file by path suffix: its path in
    ///    the graph is not reliable (the indexer names an anonymous namespace after
    ///    an identifier inside it), and it hides every alias of another file;
    /// 3. the declaration the innermost scope holds, when no scope between it and
    ///    the caller can supply another (`unambiguous_from`);
    /// 4. the suffix reading, for a name no enclosing scope declares (a
    ///    using-declaration may bring it in) or one a scope between may supply.
    pub(super) fn family_in(&self, name: &str, caller: &Caller) -> Family {
        let exact = self.declared_in(name, caller);
        let own_alias = |path: &str| {
            self.aliases_of(name)
                .any(|a| a.file == caller.file && a.path == path)
        };
        let exact_family = |path: &str| self.family(&format!("::{path}"), caller.file);
        if let Some((path, depth)) = &exact {
            if own_alias(path) && self.unambiguous_from(name, caller, *depth) {
                return exact_family(path);
            }
        }
        let written_here = |a: &Alias| a.file == caller.file && names_class(&a.path, name);
        if self.aliases_of(name).any(written_here) {
            return self.family(name, caller.file);
        }
        match exact {
            Some((path, depth)) if self.unambiguous_from(name, caller, depth) => {
                exact_family(&path)
            }
            _ => self.family(name, caller.file),
        }
    }

    fn aliases_of<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Alias> {
        let last = name.rsplit("::").next().unwrap_or(name);
        self.aliases.get(last).into_iter().flatten()
    }

    /// The full path of the class or alias `name` names from inside the caller's
    /// scope, and the number of scope segments it sits under: the innermost
    /// `scope[..k]::name` that the graph holds and the caller's file can see.
    fn declared_in(&self, name: &str, caller: &Caller) -> Option<(String, usize)> {
        if name.starts_with("::") {
            return None;
        }
        let last = name.rsplit("::").next().unwrap_or(name);
        let holds = |path: &str| {
            self.by_last
                .get(last)
                .is_some_and(|paths| paths.iter().any(|p| p == path))
                && self.class_seen_from(path, caller.file)
                || self.aliases.get(last).is_some_and(|aliases| {
                    aliases
                        .iter()
                        .any(|a| a.path == path && sees(caller.file, &a.file))
                })
        };
        (0..=caller.scope.len()).rev().find_map(|k| {
            let path = caller.scope[..k]
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(name))
                .collect::<Vec<_>>()
                .join("::");
            holds(&path).then_some((path, k))
        })
    }

    /// True when a declaration of `class` is in `from`, or in a file any file sees.
    fn class_seen_from(&self, class: &str, from: &str) -> bool {
        self.class_files
            .get(class)
            .is_none_or(|files| files.iter().any(|file| sees(from, file)))
    }

    /// True when the declaration of `name` found under `depth` scope segments is the
    /// one C++ reaches, whatever the graph does not hold. A scope between the
    /// declaring one and the caller can supply another: a namespace through a
    /// using-declaration, a class through a member type it or a base declares or a
    /// using-declaration of a base's. Only the caller's own class is known to be a
    /// class (the other segments may be namespaces), and one of its bases the graph
    /// does not hold may declare the name.
    fn unambiguous_from(&self, name: &str, caller: &Caller, depth: usize) -> bool {
        if depth == caller.scope.len() {
            return true;
        }
        if !caller.in_class || depth + 1 != caller.scope.len() {
            return false;
        }
        let own = format!("::{}", caller.scope.join("::"));
        let family = self.family(&own, caller.file);
        family.0.iter().all(|base| {
            let classes: Vec<&String> = self
                .by_last
                .get(base.rsplit("::").next().unwrap_or(base))
                .into_iter()
                .flatten()
                .filter(|path| names_class(path, base))
                .collect();
            !classes.is_empty()
                && classes
                    .iter()
                    .all(|class| !self.declares_member(class, name))
        })
    }

    /// True when the class of path `class` declares a member type `name`.
    fn declares_member(&self, class: &str, name: &str) -> bool {
        let member = format!("{class}::{name}");
        let last = name.rsplit("::").next().unwrap_or(name);
        self.by_last
            .get(last)
            .is_some_and(|paths| paths.contains(&member))
            || self
                .aliases
                .get(last)
                .is_some_and(|aliases| aliases.iter().any(|a| a.path == member))
    }
}
