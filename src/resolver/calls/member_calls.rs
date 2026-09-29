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
//   class and of its bases can be named;
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

use super::reason::{
    Decline, Gated, SCOPE_CPP_QUALIFIER, SCOPE_CPP_RECEIVER_CLASS, SCOPE_CPP_UNQUALIFIED_CALL,
};
use super::*;
use crate::graph_store::{
    calls_through_a_pointer, GraphStore, CALLEE_SHAPE_DIRECT, CALLEE_SHAPE_INDIRECT,
    CALLEE_SHAPE_MEMBER, RECEIVER_HINT_VIA_CPP_DECLARED, RECEIVER_HINT_VIA_CPP_QUALIFIER,
    RECEIVER_HINT_VIA_CPP_THIS,
};
use crate::language_provider::extract_file_prefix;

/// The C++ classes of the graph: where each is, and the base classes it names.
#[derive(Default)]
pub(super) struct CppClasses {
    /// The path of a class (its qualified name without the file) to the raw
    /// names of its bases.
    bases: HashMap<String, Vec<String>>,
    /// The paths of the classes ending in a given last segment.
    by_last: HashMap<String, Vec<String>>,
}

impl CppClasses {
    /// Reads the `bases` of every C++ class (the `Struct` nodes of the
    /// language). A graph without the table, or without the column, yields none.
    pub(super) fn load(store: &GraphStore) -> Self {
        let mut classes = Self::default();
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
            let bases: Vec<String> = bases
                .split(',')
                .map(base_name)
                .filter(|b| !b.is_empty())
                .collect();
            let last = path.rsplit("::").next().unwrap_or(&path).to_string();
            classes.by_last.entry(last).or_default().push(path.clone());
            classes.bases.entry(path).or_default().extend(bases);
        }
        classes
    }

    /// The class named `name` (a path, possibly relative to a namespace) and
    /// every base of it, through any depth.
    fn family(&self, name: &str) -> Family {
        let mut names: HashSet<String> = HashSet::new();
        let mut pending = vec![name.to_string()];
        while let Some(current) = pending.pop() {
            if current.is_empty() || !names.insert(current.clone()) {
                continue;
            }
            let last = current.rsplit("::").next().unwrap_or(&current);
            for path in self.by_last.get(last).into_iter().flatten() {
                if names_class(path, &current) {
                    pending.extend(self.bases.get(path).into_iter().flatten().cloned());
                }
            }
        }
        Family(names)
    }
}

/// A set of class names, each a full path or a relative one.
struct Family(HashSet<String>);

impl Family {
    /// True when `class`, a full path, is one of the names.
    fn holds(&self, class: &str) -> bool {
        self.0.iter().any(|name| names_class(class, name))
    }
}

/// True when `name`, written as a path or relative to a namespace, designates
/// the class of full path `path`.
fn names_class(path: &str, name: &str) -> bool {
    path == name || path.strip_suffix(name).is_some_and(|p| p.ends_with("::"))
}

/// `Base` of `public Base<T>`, `::ns::Base` or `ns::Base`: the base as a path.
fn base_name(raw: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::new();
    for c in raw.trim().chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    let name = out.trim_start_matches("::");
    let name = name
        .strip_prefix("public")
        .or_else(|| name.strip_prefix("protected"))
        .or_else(|| name.strip_prefix("private"))
        .or_else(|| name.strip_prefix("virtual"))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(name);
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
    let typed = via == RECEIVER_HINT_VIA_CPP_THIS || via == RECEIVER_HINT_VIA_CPP_DECLARED;
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
            let family = ctx.cpp.family(site.receiver_hint);
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
        Some(class) => ctx.cpp.family(&class),
        None => Family(HashSet::new()),
    }
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

/// `a::b::f(x)`: the methods of a class the qualifier does not designate are
/// not named. What is no method is left to the general rules.
fn qualified(
    ctx: &ResolveContext,
    site: &CallSite,
    candidates: &[SymbolEntry],
) -> Vec<SymbolEntry> {
    let family = ctx.cpp.family(site.receiver_hint);
    candidates
        .iter()
        .filter(|c| {
            !is_method(c) || owner_of(&c.qualified_name).is_some_and(|owner| family.holds(&owner))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_method_belongs_to_the_class_of_its_qualified_name() {
        assert_eq!(
            owner_of("b.cpp::etl::bloom::width#1").as_deref(),
            Some("etl::bloom")
        );
        assert_eq!(owner_of("b.cpp::freefn#2").as_deref(), None);
    }

    #[test]
    fn a_class_is_named_by_its_path_or_by_a_suffix_of_it() {
        assert!(names_class("etl::bloom", "bloom"));
        assert!(names_class("etl::bloom", "etl::bloom"));
        assert!(!names_class("etl::bloom", "loom"));
        assert!(!names_class("etl::bloom", "other::bloom"));
    }

    #[test]
    fn a_base_is_read_without_access_specifier_generics_or_root() {
        assert_eq!(base_name(" public Base<T> "), "Base");
        assert_eq!(base_name("::ns::Base"), "ns::Base");
        assert_eq!(base_name("virtual ns::Base<A, B>"), "ns::Base");
        assert_eq!(base_name("private"), "private");
    }

    #[test]
    fn a_family_holds_the_bases_at_any_depth_and_survives_a_cycle() {
        let mut classes = CppClasses::default();
        for (path, bases) in [("ns::a", "b"), ("ns::b", "c"), ("ns::c", "a")] {
            let last = path.rsplit("::").next().unwrap().to_string();
            classes
                .by_last
                .entry(last)
                .or_default()
                .push(path.to_string());
            classes
                .bases
                .insert(path.to_string(), vec![bases.to_string()]);
        }
        let family = classes.family("a");
        assert!(family.holds("ns::a") && family.holds("ns::b") && family.holds("ns::c"));
        assert!(!family.holds("ns::d"));
    }
}
