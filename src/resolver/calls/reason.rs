// resolver::calls::reason: why a call site the static resolver leaves open is
// open (issue #393).
//
// One reason per site, from the closed set of `graph_store::callsite_reasons`,
// the first that applies in this order: the file no Cargo target compiles,
// twins the build does not decide between, a callee proven to live outside the
// repository, a scope rule that declined, several candidates, a method call on
// a receiver of no known type, a callee whose name the repository holds but
// not for this call, and last a callee nothing is known about. The macro pass
// gives the macro reasons (`resolver_layers`).
//
// `external_callee` needs evidence, never absence alone: the std or core root
// the callee's path is written or imported from, a name of the Rust prelude or
// of the standard-library table the repository does not define, a receiver type
// of that table, a primitive type, or a crate root the recorded Cargo facts do
// not list as a library of the repository.
// source: The Rust Reference, "Names" (extern prelude, language prelude);
// std::prelude (edition 2021); stdlib_index::rust (the curated std table).

use super::*;
use crate::graph_store::callsite_reasons as reasons;

/// Why a gate or a filter declined a call it could have tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decline {
    /// A receiver type imported from a crate the recorded Cargo facts show is
    /// not a library of this repository.
    ForeignReturnType,
    /// A scope rule refused every candidate: the rule.
    Scope(&'static str),
}

pub(super) const SCOPE_UNKNOWN_CARGO_FACTS: &str = "unknown_cargo_facts";
pub(super) const SCOPE_TYPE_ALIAS_RETURN: &str = "type_alias_return";
pub(super) const SCOPE_CRATE: &str = "crate_scope";
pub(super) const SCOPE_WRITTEN_PATH: &str = "written_path";
pub(super) const SCOPE_VARIANT_GUARD: &str = "variant_guard";

/// A resolution and, when the call was declined, why.
pub(super) type Gated = (PolicyResolution<SymbolEntry>, Option<Decline>);

/// How a call failed to become an edge.
pub(super) enum Failure<'a> {
    NotFound(Option<Decline>),
    Ambiguous {
        count: usize,
        twins: bool,
    },
    /// A target was found, but the schema has no table for the label pair.
    NoRelTable(&'a str),
}

/// Roots whose paths name the standard library.
const STD_ROOTS: [&str; 3] = ["std", "core", "alloc"];

/// The names the Rust 2021 prelude brings into every module.
/// source: https://doc.rust-lang.org/std/prelude/index.html (v1 and rust_2021).
const PRELUDE: [&str; 39] = [
    "Copy",
    "Send",
    "Sized",
    "Sync",
    "Unpin",
    "Drop",
    "Fn",
    "FnMut",
    "FnOnce",
    "drop",
    "Box",
    "ToOwned",
    "Clone",
    "PartialEq",
    "PartialOrd",
    "Eq",
    "Ord",
    "AsRef",
    "AsMut",
    "Into",
    "From",
    "Default",
    "Iterator",
    "Extend",
    "IntoIterator",
    "DoubleEndedIterator",
    "ExactSizeIterator",
    "Option",
    "Some",
    "None",
    "Result",
    "Ok",
    "Err",
    "String",
    "ToString",
    "Vec",
    "TryFrom",
    "TryInto",
    "FromIterator",
];

/// The primitive types of the language, which no `use` can name.
/// source: The Rust Reference, "Types" (boolean, numeric, textual types).
const PRIMITIVES: [&str; 17] = [
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64",
];

/// The reason and detail of a site `failure` left open.
pub(super) fn classify(
    ctx: &ResolveContext,
    site: &CallSite,
    file_id: &str,
    failure: &Failure,
) -> (&'static str, String) {
    if ctx.evidence.outside_targets.contains(file_id) {
        return (reasons::REASON_OUTSIDE_TARGETS, String::new());
    }
    if let Failure::NoRelTable(label) = failure {
        // A constant or a module named where a function is passed by value:
        // the site references an item that is not callable.
        if !matches!(*label, "Function" | "Method" | "Struct") {
            return (reasons::REASON_NOT_A_CALL, format!("names_{label}"));
        }
        return (reasons::REASON_NOT_FOUND, format!("no_rel_table:{label}"));
    }
    if names_repo_variant(ctx, site) {
        return (reasons::REASON_NOT_A_CALL, "enum_variant".to_string());
    }
    if matches!(failure, Failure::Ambiguous { twins: true, .. }) {
        return (reasons::REASON_CFG_TWINS, String::new());
    }
    let decline = match failure {
        Failure::NotFound(decline) => *decline,
        _ => None,
    };
    if let Some(origin) = external_origin(ctx, site, file_id, decline) {
        return (reasons::REASON_EXTERNAL_CALLEE, origin);
    }
    if let Some(Decline::Scope(rule)) = decline {
        return (reasons::REASON_DECLINED_BY_SCOPE, rule.to_string());
    }
    if let Failure::Ambiguous { count, .. } = failure {
        return (reasons::REASON_AMBIGUOUS, count.to_string());
    }
    let rust = ctx.provider.language() == "rust";
    if rust && site.callee.contains('.') && site.receiver_hint.is_empty() && !self_value(ctx, site)
    {
        return (reasons::REASON_NO_RECEIVER_TYPE, String::new());
    }
    if name_is_known(ctx, site) {
        return (reasons::REASON_NOT_FOUND, String::new());
    }
    (reasons::REASON_UNKNOWN_CALLEE, String::new())
}

/// `Kind::A(1)`, `Self::A(1)`: the path ends in a variant of an enum of the
/// repository, which builds a value and calls nothing. The graph holds the
/// variant, not a callable, so no call edge can name it.
fn names_repo_variant(ctx: &ResolveContext, site: &CallSite) -> bool {
    if ctx.provider.language() != "rust" {
        return false;
    }
    let Some(segments) = crate::call_evidence::callee_path_segments(site.callee) else {
        return false;
    };
    let (variant, owner) = (segments[segments.len() - 1], segments[segments.len() - 2]);
    if owner == "Self" {
        return receiver::impl_qn_of(site.caller_qn)
            .is_some_and(|impl_qn| ctx.assoc.has_variant(&format!("{impl_qn}::{variant}")));
    }
    ctx.idx.by_name.get(owner).is_some_and(|entries| {
        entries.iter().any(|e| {
            e.label == "Enum"
                && ctx
                    .assoc
                    .has_variant(&format!("{}::{variant}", e.qualified_name))
        })
    })
}

/// True for the language's own value receiver (`self.m`): its type is the
/// caller's own impl, so a failure there is a missing method, not a missing type.
fn self_value(ctx: &ResolveContext, site: &CallSite) -> bool {
    let spelling = receiver::ReceiverSpelling::of(ctx.provider);
    matches!(
        receiver::classify(site.callee, &spelling),
        receiver::ReceiverForm::SelfValue(_) | receiver::ReceiverForm::SelfType(_)
    )
}

/// The identifier the call looks up: the method of a receiver call, the last
/// segment of a path.
fn called_name(callee: &str) -> &str {
    let after_dot = callee.rsplit('.').next().unwrap_or(callee);
    after_dot.rsplit("::").next().unwrap_or(after_dot)
}

/// A callee whose name the repository holds for some item, or a receiver whose
/// type the repository defines: the item exists, only not for this call.
fn name_is_known(ctx: &ResolveContext, site: &CallSite) -> bool {
    if self_value(ctx, site) || ctx.idx.by_name.contains_key(called_name(site.callee)) {
        return true;
    }
    !site.receiver_hint.is_empty() && repo_defines_type(ctx, type_of_hint(site.receiver_hint))
}

/// The type name a receiver hint names: its last path segment, without generics.
fn type_of_hint(hint: &str) -> &str {
    let plain = receiver::strip_generics(hint);
    plain.rsplit("::").next().unwrap_or(plain)
}

/// True when the repository defines a type of that name.
fn repo_defines_type(ctx: &ResolveContext, name: &str) -> bool {
    ctx.idx.by_name.get(name).is_some_and(|entries| {
        entries
            .iter()
            .any(|e| matches!(e.label.as_str(), "Struct" | "Enum" | "Trait" | "TypeAlias"))
    })
}

/// True when the repository holds any item of that name.
fn repo_defines(ctx: &ResolveContext, name: &str) -> bool {
    ctx.idx.by_name.contains_key(name)
}

/// A type of the standard library the repository does not define: a prelude
/// name, a receiver type of the curated std table, or a primitive.
fn std_type(ctx: &ResolveContext, name: &str) -> bool {
    let known = PRIMITIVES.contains(&name)
        || PRELUDE.contains(&name)
        || crate::stdlib_index::rust::RUST_SYMBOLS
            .iter()
            .any(|s| s.receiver_type == name);
    known && !repo_defines(ctx, name)
}

/// Where the callee lives outside the repository, with evidence (see the module
/// header), or `None`. Rust only.
fn external_origin(
    ctx: &ResolveContext,
    site: &CallSite,
    file_id: &str,
    decline: Option<Decline>,
) -> Option<String> {
    if ctx.provider.language() != "rust" {
        return None;
    }
    if decline == Some(Decline::ForeignReturnType) {
        return site
            .receiver_hint_via
            .strip_prefix(crate::graph_store::RECEIVER_HINT_VIA_IMPORT_PREFIX)
            .map(str::to_string);
    }
    if site.callee.contains('.') {
        return hinted_origin(ctx, site);
    }
    if site.callee.contains("::") {
        return path_origin(ctx, file_id, site.callee);
    }
    (PRELUDE.contains(&site.callee) && !repo_defines(ctx, site.callee)).then(|| "std".to_string())
}

/// True when a Rust path callee (`Vec::new`, `io::BufWriter::new`,
/// `rand::random`) is proven to live outside the repository: no repository item
/// can be its target, whatever its last segment matches. Without this, a lone
/// `Set::new` of the repository took `Vec::new` by its bare name `new`.
pub(super) fn names_external_path(ctx: &ResolveContext, site: &CallSite, file_id: &str) -> bool {
    ctx.provider.language() == "rust" && path_origin(ctx, file_id, site.callee).is_some()
}

/// A receiver call whose receiver type is written through std, or is a std
/// type the repository does not define.
fn hinted_origin(ctx: &ResolveContext, site: &CallSite) -> Option<String> {
    let hint = receiver::strip_generics(site.receiver_hint);
    if hint.is_empty() {
        return None;
    }
    let root = hint.split("::").next().unwrap_or(hint);
    if STD_ROOTS.contains(&root) {
        return Some(root.to_string());
    }
    std_type(ctx, type_of_hint(hint)).then(|| "std".to_string())
}

/// A path callee rooted in std, in a crate of no library of the repository, or
/// in a std type the repository does not define.
fn path_origin(ctx: &ResolveContext, file_id: &str, callee: &str) -> Option<String> {
    let segments = crate::call_evidence::callee_path_segments(callee)?;
    let root = segments[0];
    if matches!(root, "crate" | "self" | "super" | "Self") {
        return None;
    }
    let imported = imported_origin(ctx, file_id, root);
    let origin = imported.unwrap_or(root);
    if STD_ROOTS.contains(&origin) {
        return Some(origin.to_string());
    }
    if matches!(origin, "crate" | "self" | "super") || ctx.evidence.crate_names.contains(origin) {
        return None;
    }
    // `Vec::new`: the type the call is associated with is a std type the
    // repository does not define, written with no path before it.
    let qualifier = crate::call_evidence::callee_names_type(callee);
    if imported.is_none() && segments.len() == 2 && qualifier.is_some_and(|q| std_type(ctx, q)) {
        return Some("std".to_string());
    }
    // A crate of the extern prelude: named by a `use`, or written directly with
    // a lowercase root the repository holds no item for.
    let written_crate = origin.starts_with(|c: char| c.is_ascii_lowercase());
    let foreign =
        ctx.evidence.known && !repo_defines(ctx, origin) && (imported.is_some() || written_crate);
    foreign.then(|| origin.to_string())
}

/// The first segment of the `use` path of `file_id` that binds `name`
/// (`use std::io;` binds `io` to `std`), or `None` when no `use` binds it.
fn imported_origin<'a>(ctx: &'a ResolveContext, file_id: &str, name: &str) -> Option<&'a str> {
    let imports = ctx.file_imports.get(file_id)?;
    imports.iter().find_map(|path| {
        let path = path.strip_suffix("::self").unwrap_or(path);
        let last = path.rsplit("::").next()?;
        (last == name && path.contains("::")).then(|| path.split("::").next())?
    })
}

#[cfg(test)]
#[path = "reason_tests.rs"]
mod tests;
