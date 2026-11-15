// resolver::calls::member_calls::scope_lookup — the type of a C++ parameter, looked
// up as C++ does in the scopes that enclose the caller (issue #412 point 3).
//
// source: ISO/IEC 14882:2017 §6.4.1 [basic.lookup.unqual] ¶6 and ¶8, §6.4.3
// [basic.lookup.qual].

use super::{extract_file_prefix, Alias, CppClasses, Family, GraphStore};

/// What the scope lookup of a parameter's type found: a class of that exact path,
/// or the classes the aliases of that exact path name.
enum Found {
    Class(String),
    Aliases(Vec<String>),
}

impl CppClasses {
    /// Reads the C++ `using` declarations and directives: an `Import` whose id holds
    /// `::using:` (`parser::spec::cpp::using_entry`), its text after that marker.
    pub(super) fn load_usings(&mut self, store: &GraphStore) {
        let Ok(qr) = store.execute_query("MATCH (i:Import) RETURN i.id") else {
            return;
        };
        for row in qr.rows.iter().filter(|r| !r.is_empty()) {
            if let Some((_, text)) = row[0].split_once("::using:") {
                let file = extract_file_prefix(&row[0]).unwrap_or_default();
                self.usings.push((file, text.trim().to_string()));
            }
        }
    }

    /// The family of the type a parameter of a caller of scope `caller_scope` writes
    /// as `name`: the class or the aliases `look_up` finds, and their bases; when it
    /// finds none, the path-suffix reading of `family`.
    pub(super) fn family_in_scope(&self, name: &str, caller_scope: &str, from: &str) -> Family {
        match self.look_up(name, caller_scope, from) {
            Some(Found::Class(path)) => {
                let bases = self.bases.get(&path).cloned().unwrap_or_default();
                let mut family = Family::default();
                family.exact.insert(path);
                self.grow(family, bases, from)
            }
            Some(Found::Aliases(targets)) => self.grow(Family::default(), targets, from),
            None => self.family(name, from),
        }
    }

    /// Unqualified lookup of a type name written in a parameter, from the scope that
    /// encloses the function (`a::C` for `a::C::f`) outward to the global scope; a
    /// name written `::X` is looked up in the global scope only. A qualified name
    /// (`ns::Box`) is tried under each scope as a whole. `None` when the graph cannot
    /// show what C++ reaches: a `using` that may bring the first segment in
    /// (`using_may_name`), a class scope with bases and no member of that name (the
    /// member may be inherited), or no scope holding the name.
    /// source: ISO/IEC 14882:2017 §6.4.1 [basic.lookup.unqual] ¶6 and ¶8, §6.4.3
    /// [basic.lookup.qual] (the first segment of `ns::Box` is looked up unqualified).
    fn look_up(&self, name: &str, caller_scope: &str, from: &str) -> Option<Found> {
        let (name, scopes) = match name.strip_prefix("::") {
            Some(global) => (global, vec![String::new()]),
            None => (name, enclosing_scopes(caller_scope)),
        };
        if name.is_empty() || self.using_may_name(name, from) {
            return None;
        }
        let last = name.rsplit("::").next().unwrap_or(name);
        for scope in scopes {
            let path = if scope.is_empty() {
                name.to_string()
            } else {
                format!("{scope}::{name}")
            };
            let class = self.bases.contains_key(&path);
            let aliases: Vec<&Alias> = self
                .aliases
                .get(last)
                .into_iter()
                .flatten()
                .filter(|a| a.path == path && self.sees(from, &a.file))
                .collect();
            // `typedef struct Box Box;` names the class it sits beside.
            let other_aliases: Vec<String> = aliases
                .iter()
                .filter(|a| !(class && (a.target == path || a.target == last)))
                .map(|a| a.target.clone())
                .collect();
            // An alias another file writes is visible only when the two files may be
            // one translation unit (`sees`), which the graph cannot confirm: the
            // lookup commits to an alias the caller's own file writes only.
            let all_local = aliases.iter().all(|a| a.file == from);
            match (class, other_aliases.is_empty()) {
                (true, true) => return Some(Found::Class(path)),
                (true, false) => return None,
                (false, false) if all_local => return Some(Found::Aliases(other_aliases)),
                (false, false) => return None,
                (false, true) => {}
            }
            if self.bases.get(&scope).is_some_and(|b| !b.is_empty()) {
                return None;
            }
        }
        None
    }

    /// True when a `using` visible to `from` may make the first segment of `name`
    /// mean another entity than the scopes show: a using-declaration whose last
    /// segment is that segment (`using other::Box;`), or a using-directive whose
    /// namespace holds a class or an alias of that name (`using namespace other;`
    /// with `other::Box`). The scope a `using` is written in is not read: any one
    /// visible to the caller counts.
    fn using_may_name(&self, name: &str, from: &str) -> bool {
        let first = name.split("::").next().unwrap_or(name);
        self.usings
            .iter()
            .filter(|(file, _)| self.sees(from, file))
            .any(|(_, text)| match text.strip_prefix("namespace") {
                Some(ns) => {
                    let needle = format!("::{}::{first}::", ns.trim().trim_start_matches("::"));
                    self.bases
                        .keys()
                        .chain(self.aliases.values().flatten().map(|a| &a.path))
                        .any(|path| format!("::{path}::").contains(&needle))
                }
                None => text.rsplit("::").next().map(str::trim) == Some(first),
            })
    }
}

/// `a::C`, `a` then the global scope (empty) for the scope `a::C`.
fn enclosing_scopes(scope: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = scope;
    while !current.is_empty() {
        out.push(current.to_string());
        current = current.rsplit_once("::").map_or("", |(outer, _)| outer);
    }
    out.push(String::new());
    out
}
