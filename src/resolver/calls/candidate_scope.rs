// resolver::calls::candidate_scope: which candidates a call can name before
// any evidence is weighed. The language's scoping rules (`visible_candidates`,
// moved out of `calls.rs` to keep it under the size cap) and, for a Rust call
// written with a path (`a::dup()`, `crate::a::dup()`, `Set::new()`), the owners
// its qualifier names (`qualified_path_gate`, issue #398).

use super::reason::{Decline, Gated, SCOPE_WRITTEN_PATH};
use super::*;

/// The call resolved against the owners the qualifier of `site.callee` names,
/// among `candidates` (the lookup by name's own, scoping already applied).
///
/// postcondition: `None` when the gate does not apply (not Rust, no
/// candidate, not a plain path, no rule) or when no candidate is admitted but
/// the path may only be placed elsewhere than written: the caller keeps the
/// lookup by name. `Some` is final: one admitted candidate resolves (the
/// written path is the evidence), several are ambiguous; a path that names
/// nothing of the repository (a foreign glob or `use`, a repository type
/// without that item) declines (`written_path`, issue #393).
/// No same-file preference applies to a written path.
pub(super) fn qualified_path_gate(
    ctx: &ResolveContext,
    site: &CallSite,
    candidates: &[SymbolEntry],
) -> Option<Gated> {
    // No candidate left: the lookup by name already says why (the variant
    // guard, or nothing of that name).
    if ctx.provider.language() != "rust" || candidates.is_empty() {
        return None;
    }
    let (qualifier, name) = site.callee.rsplit_once("::")?;
    if !is_plain_path(qualifier) {
        return None;
    }
    let name = receiver::strip_generics(name);
    let facts = receiver::PathFacts {
        idx: ctx.idx,
        evidence: ctx.evidence,
        imports: ctx.imports,
    };
    // The qualifier names owners; the call names their item `name`, which a
    // `use` of the owner may bring from elsewhere (`pub use b::dup;` in `a`).
    let admits_any = |_: &str, owners: &receiver::WrittenPath| {
        let items = owners.item(&facts, name);
        candidates.iter().any(|c| items.admits(c))
    };
    let (owners, fallback) =
        match receiver::qualifier_rule(&facts, site.caller_qn, qualifier, &admits_any) {
            receiver::QualifierRule::ByName => return None,
            receiver::QualifierRule::Decline => return Some(declined()),
            receiver::QualifierRule::Path { path, fallback } => (path, fallback),
        };
    let items = owners.item(&facts, name);
    let admitted: Vec<&SymbolEntry> = candidates.iter().filter(|c| items.admits(c)).collect();
    let kept: Vec<SymbolEntry> = admitted
        .iter()
        .filter(|c| reached(ctx, c))
        .map(|c| (*c).clone())
        .collect();
    if !kept.is_empty() {
        return Some(named(kept));
    }
    // Admitted but in no file a target reaches: the lookup by name keeps deciding.
    if !admitted.is_empty() {
        return None;
    }
    let path = OwnerPath::new(&owners, qualifier, fallback);
    unadmitted(ctx, candidates, &path)
}

/// The owners a written path names, with the last segment of its qualifier.
struct OwnerPath<'a> {
    owners: &'a receiver::WrittenPath<'a>,
    owner: &'a str,
    fallback: bool,
}

impl<'a> OwnerPath<'a> {
    fn new(owners: &'a receiver::WrittenPath<'a>, qualifier: &'a str, fallback: bool) -> Self {
        let owner = receiver::strip_generics(qualifier.rsplit("::").next().unwrap_or(qualifier));
        Self {
            owners,
            owner,
            fallback,
        }
    }
}

/// A path that admits no candidate: the `impl` blocks of a type it names
/// decide first (`through_impls`); otherwise the call declines when the path
/// names nothing of the repository, and the lookup by name keeps deciding when
/// it may only be placed elsewhere than written.
fn unadmitted(ctx: &ResolveContext, candidates: &[SymbolEntry], path: &OwnerPath) -> Option<Gated> {
    if let Some(outcome) = through_impls(ctx, path.owners, candidates, path.owner) {
        return outcome;
    }
    let decline = lacks_the_item(ctx, candidates, path.owner)
        || (!path.fallback && path.owners.leaves_repository(ctx.imports));
    decline.then(declined)
}

/// The labels of a type an `impl` block can be written for.
const TYPE_LABELS: [&str; 5] = ["Struct", "Enum", "Union", "TypeAlias", "Trait"];

/// The call of an item of a type the path names, through the `impl` blocks of
/// that type placed in another module (`impl Gc` under `use crate::g::Gc;`,
/// `impl crate::g::Gc`): the candidates whose owner reads, in its own file, as
/// a type the path names. Another module's own type of the same name is not
/// that type, so a path to a type without the item declines.
///
/// postcondition: `None` when the path names no type of the repository (the
/// caller's rules apply); `Some(None)` when the owner of a candidate cannot be
/// read (the lookup by name decides); `Some(Some(_))` is final.
fn through_impls(
    ctx: &ResolveContext,
    owners: &receiver::WrittenPath,
    candidates: &[SymbolEntry],
    owner: &str,
) -> Option<Option<Gated>> {
    let facts = receiver::PathFacts {
        idx: ctx.idx,
        evidence: ctx.evidence,
        imports: ctx.imports,
    };
    let types: Vec<&SymbolEntry> = ctx
        .idx
        .by_name
        .get(owner)?
        .iter()
        .filter(|e| TYPE_LABELS.contains(&e.label.as_str()) && owners.names(e))
        .collect();
    if types.is_empty() {
        return None;
    }
    let path_types = NamedTypes { name: owner, types };
    let mut matched = Vec::new();
    for candidate in candidates {
        match impl_owner_is_named(ctx, &facts, candidate, &path_types) {
            Some(true) if reached(ctx, candidate) => matched.push(candidate.clone()),
            Some(_) => {}
            None => return Some(None),
        }
    }
    Some(Some(if matched.is_empty() {
        declined()
    } else {
        named(matched)
    }))
}

/// The types of one name a written path names.
struct NamedTypes<'a> {
    name: &'a str,
    types: Vec<&'a SymbolEntry>,
}

/// Whether the owner of `candidate` (the type its `impl` block writes) is one
/// of `named`, read in the candidate's own file.
///
/// postcondition: `None` only when the owner cannot be read, that is when the
/// candidate's parent does not sit under its own file's scope (the two
/// `strip_prefix` below). Every other answer is `Some`: an owner that resolves
/// to another type, or to nothing the file defines or imports, is `Some(false)`.
fn impl_owner_is_named(
    ctx: &ResolveContext,
    facts: &receiver::PathFacts,
    candidate: &SymbolEntry,
    named: &NamedTypes,
) -> Option<bool> {
    let Some((parent, _)) = candidate.qualified_name.rsplit_once("::") else {
        return Some(false);
    };
    let owner = receiver::strip_generics(parent.rsplit("::").next().unwrap_or(parent));
    if owner != named.name {
        return Some(false);
    }
    let scope = receiver::caller_scope(ctx.idx, &candidate.qualified_name);
    // Unreachable today: `caller_scope` keeps a prefix of this same name's segments.
    let written = parent.strip_prefix(&scope)?.strip_prefix("::")?;
    let names = |path: &receiver::WrittenPath| named.types.iter().any(|t| path.names(t));
    if written.contains("::") {
        let path = receiver::WrittenPath::of(facts, &candidate.qualified_name, written);
        return Some(path.is_some_and(|p| names(&p)));
    }
    let admits = |_: &str, path: &receiver::WrittenPath| names(path);
    match receiver::bind(
        facts,
        &candidate.qualified_name,
        owner,
        &admits,
        receiver::Defines::Types,
    ) {
        receiver::Binding::Path { path, .. } => Some(names(&path)),
        receiver::Binding::Decline => Some(false),
        receiver::Binding::ByName => Some(local_type_is_named(ctx, &scope, named)),
    }
}

/// Whether `scope` itself defines a type of that name that is one of `named`.
/// A scope that defines no such type answers false, a readable answer: the
/// owner it writes is then some other module's type (reached by a glob that
/// does not bring any of `named`), not the type the path names. Answering
/// "unreadable" here would hand the call back to the lookup by name, which
/// binds it to that other type's item.
fn local_type_is_named(ctx: &ResolveContext, scope: &str, named: &NamedTypes) -> bool {
    let local = format!("{scope}::{}", named.name);
    let defined = ctx
        .idx
        .by_name
        .get(named.name)
        .is_some_and(|entries| entries.iter().any(|e| e.qualified_name == local));
    defined && named.types.iter().any(|t| t.qualified_name == local)
}

/// True when `owner` is a type of the repository and no candidate is an item
/// of any owner of that name (a derived `IndexOptions::default`, a method a
/// foreign trait gives): the call names nothing the repository defines. When
/// some candidate has an owner of that name the path is only placed elsewhere
/// than written (an `impl` in another module, a `#[path]` file), and the lookup
/// by name keeps deciding.
fn lacks_the_item(ctx: &ResolveContext, candidates: &[SymbolEntry], owner: &str) -> bool {
    let is_type = ctx.idx.by_name.get(owner).is_some_and(|entries| {
        entries
            .iter()
            .any(|e| TYPE_LABELS.contains(&e.label.as_str()))
    });
    let of_owner = |c: &SymbolEntry| {
        c.qualified_name
            .rsplit_once("::")
            .is_some_and(|(parent, _)| parent.rsplit("::").next() == Some(owner))
    };
    is_type && !candidates.iter().any(of_owner)
}

/// False for a file no module tree of a target reaches (a stray file at the
/// place a `#[path]` moved its module from): it holds nothing a path can
/// name. The Cargo facts alone never decline the call: without them every
/// file counts, and a candidate dropped here leaves the lookup by name.
fn reached(ctx: &ResolveContext, candidate: &SymbolEntry) -> bool {
    let file = extract_file_prefix_or_self(&candidate.qualified_name);
    !ctx.evidence.known
        || (ctx.evidence.owners.contains_key(&file)
            && !ctx.evidence.outside_targets.contains(&file))
}

/// The one candidate a path names resolves, with the written path as the
/// evidence; several stay ambiguous.
fn named(mut kept: Vec<SymbolEntry>) -> Gated {
    if kept.len() > 1 {
        return (PolicyResolution::Ambiguous { candidates: kept }, None);
    }
    let evidence = ambiguity_policy::Evidence::ImportMatch;
    (
        PolicyResolution::Resolved {
            target: kept.remove(0),
            evidence,
            confidence: ambiguity_policy::confidence_for(evidence),
        },
        None,
    )
}

/// The written path names no candidate of the repository.
fn declined() -> Gated {
    (
        PolicyResolution::NotFound,
        Some(Decline::Scope(SCOPE_WRITTEN_PATH)),
    )
}

/// True when `qualifier` is a path of plain segments (`a`, `crate::a`,
/// `Vec::<u8>`): no receiver expression, call, macro or qualified-self
/// (`<T as Tr>`) form.
fn is_plain_path(qualifier: &str) -> bool {
    !qualifier.is_empty()
        && !qualifier.starts_with('<')
        && ![".", "(", ")", "!", "[", "{"]
            .iter()
            .any(|mark| qualifier.contains(mark))
}

/// The candidates a call at `site` can actually name under the caller
/// language's scoping rules, before any evidence is weighed.
///
/// Rust block-scoped fn items (issue #327): a nested fn shadows every other
/// candidate inside its enclosing callable and is invisible outside. Python
/// (`bare_call_binds_methods == false`): an unqualified call never names a
/// method, so a same-named method is not a rival of the module function it
/// really calls (pg_store.py `_now_iso()`, which the #30 policy used to drop
/// as ambiguous, #335) and is never a target on its own.
pub(super) fn visible_candidates<'a>(
    ctx: &ResolveContext,
    site: &CallSite,
    candidates: &'a [SymbolEntry],
    qualified: bool,
) -> std::borrow::Cow<'a, [SymbolEntry]> {
    use std::borrow::Cow;
    if ctx.provider.language() == "rust" {
        return Cow::Owned(nested_scope::visible_candidates(
            ctx.idx,
            candidates,
            site.caller_qn,
            qualified,
        ));
    }
    if qualified || ctx.provider.bare_call_binds_methods() {
        return Cow::Borrowed(candidates);
    }
    Cow::Owned(
        candidates
            .iter()
            .filter(|c| c.label != "Method")
            .cloned()
            .collect(),
    )
}
