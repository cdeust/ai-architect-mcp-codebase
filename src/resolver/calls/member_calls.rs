// resolver::calls::member_calls: which C++ methods a call can name (issue #406).
//
// The C++ parser reduces every callee to its last identifier, so `p->empty()`,
// `ru.empty()` and `width(1U)` all reach the resolver as a bare name. Binding
// that name to any method of the repository made a call on an unknown receiver
// name whichever class held the name. What the source states is kept as the
// site's `receiver_hint` and `receiver_hint_via` (`parser::spec::cpp_receiver`);
// this module reads it:
//
// - a member call (`a.f()`, `p->f()`) on a receiver whose type the parser did
//   not read stays open as `no_receiver_type` (`open_call`);
// - through `this` or a receiver of a declared type, only the methods of that
//   class and of its bases can be named. The type of a parameter of a function
//   defined outside any function body (`cpp-parameter`) is looked up as C++ does,
//   in the scopes that enclose the caller, innermost first (`look_up`): the first
//   scope that holds a class or an alias of that exact path names it. Where the
//   graph does not show what C++ would reach the lookup gives up and the path-suffix
//   reading applies, as before #412: a `using` visible to the caller that may bring
//   the name in, a class scope with bases (a member type may be inherited), or no
//   scope holding the name. Any other declared type is read by path suffix
//   (`family`): one class of that name binds, several keep the site open. A block
//   declaration (a local class, a block `typedef`) has no node, which is why a
//   receiver declared in a body keeps the suffix reading. A `typedef` or
//   `using` a source file writes is visible to a file that can be compiled with it in
//   one translation unit (it includes it, it is included by it, or a third file includes
//   both) and to any file when it is written in a header;
// - an unqualified call names, first, a method of the caller's own class or of
//   one of its bases (implicit `this`, which hides every namesake outside the
//   class), then a function or a constructor; never a method of another class;
// - a qualified call `a::f()` names no method of a class `a` does not designate.
//
// The graph holds no arity and no field or global types, so overloads of one
// class stay ambiguous, and a member call on a field or a global stays open.
//
// source: ISO/IEC 14882:2017 §6.4.1 (unqualified name lookup: a class member
// hides a namespace-scope namesake), §12.2.2 (implicit `this`), §13.3 (overload
// resolution needs the argument types), §10 (derived classes).

use std::collections::{HashMap, HashSet};

use super::includes::IncludeGraph;
use super::reason::{
    Decline, Gated, SCOPE_CPP_QUALIFIER, SCOPE_CPP_RECEIVER_CLASS, SCOPE_CPP_UNQUALIFIED_CALL,
};
use super::*;
use crate::graph_store::{
    calls_through_a_pointer, GraphStore, CALLEE_SHAPE_DIRECT, CALLEE_SHAPE_INDIRECT,
    CALLEE_SHAPE_MEMBER, RECEIVER_HINT_VIA_CPP_DECLARED, RECEIVER_HINT_VIA_CPP_PARAMETER,
    RECEIVER_HINT_VIA_CPP_QUALIFIER, RECEIVER_HINT_VIA_CPP_THIS,
};
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
    /// Which file includes which (a source file is seen by the files that include it).
    includes: IncludeGraph,
    /// The `using` declarations and directives (`Import` nodes with `using`): the
    /// file that writes each and its text (`other::Box`, `namespace other`).
    usings: Vec<(String, String)>,
}

/// What the scope lookup of a parameter's type found: a class of that exact path,
/// or the classes the aliases of that exact path name.
enum Found {
    Class(String),
    Aliases(Vec<String>),
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
        classes
    }

    /// Reads the C++ `using` declarations and directives: an `Import` whose id holds
    /// `::using:` (`parser::spec::cpp::using_entry`), its text after that marker.
    fn load_usings(&mut self, store: &GraphStore) {
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
    fn family(&self, name: &str, from: &str) -> Family {
        self.grow(Family::default(), vec![name.to_string()], from)
    }

    /// The family of the type a parameter of a caller of scope `caller_scope` writes
    /// as `name`: the class or the aliases `look_up` finds, and their bases; when it
    /// finds none, the path-suffix reading of `family`.
    fn family_in_scope(&self, name: &str, caller_scope: &str, from: &str) -> Family {
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

    /// Adds to `family` the names `pending` holds, the classes they designate, the
    /// classes their aliases stand for and every base, through any depth.
    fn grow(&self, mut family: Family, mut pending: Vec<String>, from: &str) -> Family {
        while let Some(current) = pending.pop() {
            if current.is_empty() || !family.relative.insert(current.clone()) {
                continue;
            }
            let last = current.rsplit("::").next().unwrap_or(&current);
            for path in self.by_last.get(last).into_iter().flatten() {
                if names_class(path, &current) {
                    pending.extend(self.bases.get(path).into_iter().flatten().cloned());
                }
            }
            pending.extend(self.alias_targets(last, &current, from));
        }
        family
    }

    fn alias_targets(&self, last: &str, name: &str, from: &str) -> Vec<String> {
        let named: Vec<&Alias> = self
            .aliases
            .get(last)
            .into_iter()
            .flatten()
            .filter(|a| names_class(&a.path, name) && self.sees(from, &a.file))
            .collect();
        let local: Vec<&Alias> = named.iter().copied().filter(|a| a.file == from).collect();
        let chosen = if local.is_empty() { named } else { local };
        chosen.into_iter().map(|a| a.target.clone()).collect()
    }
}

/// A set of class names: `exact` full paths, each naming one class, and
/// `relative` names, each a full path or one relative to a namespace.
#[derive(Default)]
struct Family {
    exact: HashSet<String>,
    relative: HashSet<String>,
}

impl Family {
    /// True when `class`, a full path, is one of the names.
    fn holds(&self, class: &str) -> bool {
        self.exact.contains(class) || self.relative.iter().any(|name| names_class(class, name))
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

/// True when `name`, written as a path or relative to a namespace, designates
/// the class of full path `path`.
fn names_class(path: &str, name: &str) -> bool {
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
fn path_without_file(qn: &str) -> &str {
    match extract_file_prefix(qn) {
        Some(file) => qn[file.len()..].trim_start_matches("::"),
        None => qn,
    }
}

/// The class a method belongs to, from its qualified name.
fn owner_of(method_qn: &str) -> Option<String> {
    let path = path_without_file(method_qn);
    let path = path.rsplit_once('#').map_or(path, |(name, _)| name);
    path.rsplit_once("::").map(|(owner, _)| owner.to_string())
}

fn is_method(candidate: &SymbolEntry) -> bool {
    candidate.label == "Method"
}

/// A method whose name is its class's: a constructor.
fn is_constructor(candidate: &SymbolEntry, callee: &str) -> bool {
    is_method(candidate)
        && owner_of(&candidate.qualified_name)
            .is_some_and(|owner| owner.rsplit("::").next() == Some(callee))
}

/// The methods of `candidates` whose class is in `family`.
fn methods_of(family: &Family, candidates: &[SymbolEntry]) -> Vec<SymbolEntry> {
    candidates
        .iter()
        .filter(|c| {
            is_method(c) && owner_of(&c.qualified_name).is_some_and(|owner| family.holds(&owner))
        })
        .cloned()
        .collect()
}

/// A site the resolver leaves open without looking for a target: a call through
/// a function pointer (issue #401), or a C++ member call whose receiver type the
/// parser did not read (issue #406). `None` for the other sites, and for a graph
/// written before the shape column (`shape` is empty).
pub(super) fn open_call(language: &str, shape: &str, via: &str) -> Option<Gated> {
    if calls_through_a_pointer(language, shape) {
        let shape = if shape == CALLEE_SHAPE_INDIRECT {
            CALLEE_SHAPE_INDIRECT
        } else {
            CALLEE_SHAPE_MEMBER
        };
        return Some((
            PolicyResolution::NotFound,
            Some(Decline::PointerCall(shape)),
        ));
    }
    let typed = via == RECEIVER_HINT_VIA_CPP_THIS
        || via == RECEIVER_HINT_VIA_CPP_DECLARED
        || via == RECEIVER_HINT_VIA_CPP_PARAMETER;
    (language == "cpp" && shape == CALLEE_SHAPE_MEMBER && !typed)
        .then_some((PolicyResolution::NotFound, Some(Decline::NoReceiverType)))
}

/// The candidates of a C++ call that its receiver or its scope allows, and the
/// rule that left none when it did. `None` when the site is not a C++ call the
/// parser shaped: the language's general rules then apply.
pub(super) struct Scoped {
    pub(super) kept: Vec<SymbolEntry>,
    pub(super) rule: &'static str,
}

pub(super) fn scope(
    ctx: &ResolveContext,
    site: &CallSite,
    candidates: &[SymbolEntry],
) -> Option<Scoped> {
    if ctx.provider.language() != "cpp" || site.callee_shape.is_empty() {
        return None;
    }
    let (kept, rule) = match site.receiver_hint_via {
        RECEIVER_HINT_VIA_CPP_THIS => {
            let family = caller_family(ctx, site);
            (methods_of(&family, candidates), SCOPE_CPP_RECEIVER_CLASS)
        }
        RECEIVER_HINT_VIA_CPP_DECLARED => {
            let family = ctx.cpp.family(site.receiver_hint, &caller_file(site));
            (methods_of(&family, candidates), SCOPE_CPP_RECEIVER_CLASS)
        }
        RECEIVER_HINT_VIA_CPP_PARAMETER => {
            let family = ctx.cpp.family_in_scope(
                site.receiver_hint,
                &caller_scope(site),
                &caller_file(site),
            );
            (methods_of(&family, candidates), SCOPE_CPP_RECEIVER_CLASS)
        }
        RECEIVER_HINT_VIA_CPP_QUALIFIER => (qualified(ctx, site, candidates), SCOPE_CPP_QUALIFIER),
        _ if site.callee_shape == CALLEE_SHAPE_DIRECT => (
            unqualified(ctx, site, candidates),
            SCOPE_CPP_UNQUALIFIED_CALL,
        ),
        _ => return None,
    };
    Some(Scoped { kept, rule })
}

/// The caller's own class and its bases; empty when the caller is no method.
fn caller_family(ctx: &ResolveContext, site: &CallSite) -> Family {
    let owner = (site.caller_label == "Method")
        .then(|| owner_of(site.caller_qn))
        .flatten();
    match owner {
        Some(class) => ctx.cpp.family(&class, &caller_file(site)),
        None => Family::default(),
    }
}

fn caller_file(site: &CallSite) -> String {
    extract_file_prefix(site.caller_qn).unwrap_or_default()
}

/// The scope that encloses the caller: `a::C` of the method `a::C::f`, empty for
/// a global function.
fn caller_scope(site: &CallSite) -> String {
    owner_of(site.caller_qn).unwrap_or_default()
}

/// `f(x)`: a method of the caller's class or of a base hides every namesake
/// outside the class; else a function, or the constructor of a class of that
/// name.
fn unqualified(
    ctx: &ResolveContext,
    site: &CallSite,
    candidates: &[SymbolEntry],
) -> Vec<SymbolEntry> {
    let own = methods_of(&caller_family(ctx, site), candidates);
    if !own.is_empty() {
        return own;
    }
    candidates
        .iter()
        .filter(|c| !is_method(c) || is_constructor(c, site.callee))
        .cloned()
        .collect()
}

/// `a::b::f(x)`: a method must belong to the class `a::b` (or a base of it), a
/// free function to the namespace `a::b`; `::f(x)` names a global function. A
/// namespace alias is not tracked, so a call through one stays open.
fn qualified(
    ctx: &ResolveContext,
    site: &CallSite,
    candidates: &[SymbolEntry],
) -> Vec<SymbolEntry> {
    let family = ctx.cpp.family(site.receiver_hint, &caller_file(site));
    let written = site.receiver_hint.trim_start_matches("::");
    let global = site.receiver_hint.is_empty();
    candidates
        .iter()
        .filter(|c| {
            let owner = owner_of(&c.qualified_name);
            match (is_method(c), owner) {
                (true, Some(owner)) => family.holds(&owner),
                (false, Some(ns)) => !global && names_class(&ns, written),
                (false, None) => global,
                (true, None) => false,
            }
        })
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "member_calls_tests.rs"]
mod tests;
