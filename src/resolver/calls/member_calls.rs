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
//   class and of its bases can be named. A declared type written without a
//   qualifier is looked up as C++ looks it up (§6.4.1, `declared_family`): the
//   innermost scope enclosing the caller (its bases included, for a member type
//   it inherits) that holds a class of exactly that name, and that class only. No
//   enclosing scope holding one, or that class holding no candidate of the call,
//   the name is read by path suffix as before #412 (`family`): one class of that
//   name binds, several keep the site open. A `typedef` or
//   `using` a source file writes is visible to a file that can be compiled with it in
//   one translation unit (it includes it, it is included by it, or a third file includes
//   both) and to any file when it is written in a header;
// - the lookup by scope is left for the path-suffix reading wherever a
//   declaration the graph does not hold can make the name mean another class: a
//   `using` the caller sees, written in a scope at or inside the one that holds
//   the class (`a_using_can_hide`), and a declaration of the name inside the
//   caller's own body, which the parser reports as
//   `cpp-declared-local-name` (`parser::spec::cpp_declared`);
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

use super::cpp_classes::{names_class, path_without_file, Caller, Family};
use super::reason::{
    Decline, Gated, SCOPE_CPP_QUALIFIER, SCOPE_CPP_RECEIVER_CLASS, SCOPE_CPP_UNQUALIFIED_CALL,
};
use super::*;
use crate::graph_store::{
    calls_through_a_pointer, CALLEE_SHAPE_DIRECT, CALLEE_SHAPE_INDIRECT, CALLEE_SHAPE_MEMBER,
    RECEIVER_HINT_VIA_CPP_DECLARED, RECEIVER_HINT_VIA_CPP_DECLARED_LOCAL_NAME,
    RECEIVER_HINT_VIA_CPP_QUALIFIER, RECEIVER_HINT_VIA_CPP_THIS,
};
use crate::language_provider::extract_file_prefix;
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
    let typed = matches!(
        via,
        RECEIVER_HINT_VIA_CPP_THIS
            | RECEIVER_HINT_VIA_CPP_DECLARED
            | RECEIVER_HINT_VIA_CPP_DECLARED_LOCAL_NAME
    );
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
            let file = caller_file(site);
            let scope = caller_scope(site);
            let caller = Caller {
                file: &file,
                scope: &scope,
                line: call_line(site.cs_id),
            };
            let family = ctx.cpp.declared_family(site.receiver_hint, &caller);
            let mut kept = methods_of(&family, candidates);
            if kept.is_empty() {
                // The class the scopes name holds no candidate: the scopes the
                // graph models are incomplete (a base it does not hold, an
                // anonymous namespace, an alias, a declaration it has no node
                // for), so the path-suffix reading of `main` answers. The price
                // is a namesake of another namespace where the class C++ names
                // has no such method at all.
                kept = methods_of(&ctx.cpp.family(site.receiver_hint, &file), candidates);
            }
            (kept, SCOPE_CPP_RECEIVER_CLASS)
        }
        // The caller's own body declares the type's name in a form the graph
        // holds no node for: the name is read by path suffix only.
        RECEIVER_HINT_VIA_CPP_DECLARED_LOCAL_NAME => {
            let family = ctx.cpp.family(site.receiver_hint, &caller_file(site));
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

/// The line of a call, from its id (`f.cpp::f#1::call@3:22#2`); the last line
/// when the id carries none, so that no member class looks declared after it.
fn call_line(cs_id: &str) -> u64 {
    cs_id
        .rsplit_once("call@")
        .and_then(|(_, at)| at.split(':').next())
        .and_then(|line| line.parse().ok())
        .unwrap_or(u64::MAX)
}

/// The scope the caller is written in, as a path without its file: `a` of the
/// function `a::fx`, `a::Box` of the method `a::Box::m`, and the global scope
/// (empty) of a function written at file scope.
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
