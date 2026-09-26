// resolver::calls::gates: the two receiver gates `resolve_single_call` consults
// before the by-name ambiguity policy (same-class receiver, Rust local binding).
// Moved out of `calls.rs` to keep it under the size cap; behavior unchanged.

use super::*;

/// Same-class receiver call on a Method caller — Rust `self.<m>` /
/// `Self::<m>` (issue #283), Python `self.<m>` and TypeScript `this.<m>`
/// (issue #290): resolved against the caller's enclosing type BEFORE
/// `resolve_single_call`'s by-name lookup, which would otherwise try (and
/// always fail) to find a symbol literally named "self.<m>" / "this.<m>" —
/// see receiver/mod.rs's module doc. Which languages take part is decided by
/// `LanguageProvider::self_value_prefix`/`self_type_prefix`, not here.
///
/// precondition: `site.callee`/`site.caller_qn`/`site.caller_label` come
/// from the same `CallSite` row `resolve_single_call` was called with.
/// postcondition: `None` when the gate does not apply (the language has no
/// same-class receiver spelling, the caller is not a Method, or the callee
/// isn't receiver-shaped) — the caller must fall through to the
/// pre-existing by-name path. `Some(_)` is a final answer for a
/// receiver-shaped callee and is NEVER a bare-name-lookup fallback:
/// `resolve_receiver_bound` returns `Some(NotFound)` / `Some(Ambiguous)`
/// rather than `None` for those outcomes (receiver/mod.rs postcondition) —
/// this is what preserves the zero-false-callers property this module's
/// tests defend.
pub(super) fn same_class_receiver_gate(
    ctx: &ResolveContext,
    site: &CallSite,
) -> Option<PolicyResolution<SymbolEntry>> {
    let spelling = receiver::ReceiverSpelling::of(ctx.provider);
    if !spelling.binds_same_class_receiver() || site.caller_label != "Method" {
        return None;
    }
    let form = receiver::classify(site.callee, &spelling);
    if !matches!(
        form,
        receiver::ReceiverForm::SelfValue(_) | receiver::ReceiverForm::SelfType(_)
    ) {
        return None;
    }
    receiver::resolve_receiver_bound(ctx.idx, &form, receiver::impl_qn_of(site.caller_qn))
}

/// Rust `<local>.<m>` on ANY caller (not gated to `Method`, unlike the
/// `self`/`Self` gate above — a free function's local variable qualifies
/// exactly as well as a method's): resolved against the parser-attached
/// `CallSite.receiver_hint` BEFORE `resolve_single_call`'s by-name lookup,
/// which would otherwise try (and always fail) to find a symbol literally
/// named "<local>.<m>" — see receiver.rs's module doc and issue #283 palier
/// 3 (lot 6).
///
/// precondition: `site.callee`/`site.receiver_hint` come from the same
/// `CallSite` row `resolve_single_call` was called with; `file_id` is the
/// caller's own file id.
/// postcondition: `None` when the gate does not apply (non-Rust caller,
/// callee isn't `<ident>.<m>`-shaped, or `receiver_hint` is empty — no hint
/// attached, meaning the parser found no once-bound-and-typed local) — the
/// caller must fall through to the pre-existing by-name path. `Some(_)` is a
/// final answer for a hinted local receiver and is NEVER a bare-name-lookup
/// fallback, mirroring `same_class_receiver_gate`'s zero-false-callers discipline.
pub(super) fn rust_local_receiver_gate(
    ctx: &ResolveContext,
    site: &CallSite,
    file_id: &str,
) -> Option<PolicyResolution<SymbolEntry>> {
    if ctx.provider.language() != "rust" || site.receiver_hint.is_empty() {
        return None;
    }
    let constructed = site.receiver_hint_via == crate::graph_store::RECEIVER_HINT_VIA_CONSTRUCTED
        || site.receiver_hint_via == crate::graph_store::RECEIVER_HINT_VIA_CONSTRUCTED_RETURN_TYPE;
    let form = receiver::classify(site.callee, &receiver::ReceiverSpelling::of(ctx.provider));
    let m = match form {
        receiver::ReceiverForm::Local { m, .. } => m,
        // `Tier(1).join`: the receiver is an expression, so the spelling
        // analysis finds no plain identifier before the dot. Only a hint of the
        // constructed kind can come from such a receiver (issue #355).
        receiver::ReceiverForm::None if constructed => receiver::in_place_method(site.callee)?,
        _ => return None,
    };
    if let Some(resolution) = written_path_gate(ctx, site, &m) {
        return Some(resolution);
    }
    let assoc = site
        .receiver_hint_via
        .strip_prefix(crate::graph_store::RECEIVER_HINT_VIA_ASSOC_PREFIX);
    if let Some(assoc) = assoc {
        // `let x = Type::assoc(..)`: `Type` is where `assoc` lives, not what it
        // returns (issue #370). Only an owner whose `assoc` builds it counts.
        return Some(receiver::resolve_local_receiver_where(
            ctx.idx,
            site.receiver_hint,
            &m,
            file_id,
            |candidate| ctx.assoc.builds_owner(candidate, assoc),
        ));
    }
    let imported_from = site
        .receiver_hint_via
        .strip_prefix(crate::graph_store::RECEIVER_HINT_VIA_IMPORT_PREFIX);
    if imported_from.is_some_and(|root| !ctx.evidence.crate_names.contains(root)) {
        // A return type named only by a `use` of a crate the latest index pass
        // did not record as a library of this repository (issues #348, #349,
        // #358): a foreign crate's type of that name would match a namesake.
        return Some(PolicyResolution::NotFound);
    }
    let local_path = site
        .receiver_hint_via
        .strip_prefix(crate::graph_store::RECEIVER_HINT_VIA_LOCAL_IMPORT_PREFIX);
    let via_return_type = imported_from.is_some()
        || local_path.is_some()
        || site.receiver_hint_via == crate::graph_store::RECEIVER_HINT_VIA_RETURN_TYPE
        || site.receiver_hint_via == crate::graph_store::RECEIVER_HINT_VIA_CONSTRUCTED_RETURN_TYPE;
    if via_return_type && receiver::names_a_type_alias(ctx.idx, site.receiver_hint) {
        // A return type that is an alias names another type; the lookup by
        // last segment would match a namesake (issues #348 and #349).
        return Some(PolicyResolution::NotFound);
    }
    let scope = local_path.map(|path| {
        super::crate_scope::CrateScope::of(ctx.evidence, ctx.file_imports, file_id, path)
    });
    let resolution = match scope.filter(super::crate_scope::CrateScope::restricts) {
        _ if constructed => {
            receiver::resolve_local_receiver_in_file(ctx.idx, site.receiver_hint, &m, file_id)
        }
        // `use crate::X` in a test, bench, example or bin target: only the
        // candidates of the target `crate` names there (issue #357).
        Some(scope) => receiver::resolve_local_receiver_where(
            ctx.idx,
            site.receiver_hint,
            &m,
            file_id,
            |candidate| {
                scope.admits(
                    ctx.evidence,
                    &extract_file_prefix_or_self(&candidate.qualified_name),
                )
            },
        ),
        None => receiver::resolve_local_receiver_bound(ctx.idx, site.receiver_hint, &m, file_id),
    };
    Some(if via_return_type {
        receiver::relabel_as_return_type(resolution)
    } else {
        resolution
    })
}

/// How a hint that names its owner by a path is read (issues #368, #373, #380).
struct PathRule<'e> {
    /// The hint the candidates are looked up with: its last segment is the
    /// type name.
    hint: String,
    /// `None`: the path cannot be read, so the call declines.
    path: Option<receiver::WrittenPath<'e>>,
    /// The call goes back to the lookup by name when no owner matches: a
    /// cargo-less tree whose `use` starts with a name that may be this crate.
    fallback: bool,
    /// The hint was read off a return type: the weaker evidence applies.
    return_type: bool,
}

/// The receiver call resolved against the owner a path names, when the hint
/// comes with one: written in the binding (`b::Set::new()`, #368), shown by a
/// `use crate::..` for a return type (#373), or bound by a `use` (explicit or
/// glob) of the caller's module, in the order `receiver::bind` gives (#380). `None` when no path applies (the caller then keeps
/// the lookup by name). No same-file preference applies to a path.
fn written_path_gate(
    ctx: &ResolveContext,
    site: &CallSite,
    m: &str,
) -> Option<PolicyResolution<SymbolEntry>> {
    let facts = receiver::PathFacts {
        idx: ctx.idx,
        evidence: ctx.evidence,
        imports: ctx.imports,
    };
    let rule = path_rule(ctx, &facts, site, m)?;
    let assoc = site
        .receiver_hint_via
        .strip_prefix(crate::graph_store::RECEIVER_HINT_VIA_ASSOC_PREFIX);
    let resolution = match &rule.path {
        None => PolicyResolution::NotFound,
        Some(path) => receiver::resolve_local_receiver_strict(ctx.idx, &rule.hint, m, |c| {
            path.admits(c) && assoc.is_none_or(|a| ctx.assoc.builds_owner(c, a))
        }),
    };
    if rule.fallback && resolution == PolicyResolution::NotFound {
        return None;
    }
    Some(if rule.return_type {
        receiver::relabel_as_return_type(resolution)
    } else {
        resolution
    })
}

/// The path rule of `site`'s hint, as described on `written_path_gate`.
fn path_rule<'e>(
    ctx: &ResolveContext,
    facts: &receiver::PathFacts<'e>,
    site: &CallSite,
    m: &str,
) -> Option<PathRule<'e>> {
    use crate::graph_store::{
        RECEIVER_HINT_VIA_ASSOC_PREFIX as ASSOC, RECEIVER_HINT_VIA_CONSTRUCTED as CONSTRUCTED,
        RECEIVER_HINT_VIA_CONSTRUCTED_RETURN_TYPE as CONSTRUCTED_RETURN_TYPE,
        RECEIVER_HINT_VIA_LOCAL_IMPORT_PREFIX as LOCAL_IMPORT,
    };
    let (hint, via) = (site.receiver_hint, site.receiver_hint_via);
    let read_off_binding = via.is_empty() || via.starts_with(ASSOC);
    let rule = |hint: &str, path, return_type| PathRule {
        hint: hint.to_string(),
        path,
        fallback: false,
        return_type,
    };
    if hint.contains("::") {
        let path = receiver::WrittenPath::of(facts, site.caller_qn, hint);
        return read_off_binding.then(|| rule(hint, path, false));
    }
    if let Some(local) = via.strip_prefix(LOCAL_IMPORT) {
        // `use crate::X` for a return type in a module of the library (#373);
        // a test, bench, example or bin target keeps its `CrateScope` (#357).
        let file_id = extract_file_prefix_or_self(site.caller_qn);
        let scope =
            super::crate_scope::CrateScope::of(ctx.evidence, ctx.file_imports, &file_id, local);
        if !local.starts_with("crate::") || scope.restricts() {
            return None;
        }
        return Some(rule(
            local,
            receiver::WrittenPath::of(facts, site.caller_qn, local),
            true,
        ));
    }
    if !(read_off_binding || via == CONSTRUCTED || via == CONSTRUCTED_RETURN_TYPE) {
        // A return type is named in the module of its function, not the caller's.
        return None;
    }
    let assoc = via.strip_prefix(ASSOC);
    let admits_any = |hint: &str, path: &receiver::WrittenPath| {
        receiver::resolve_local_receiver_strict(ctx.idx, hint, m, |c| {
            path.admits(c) && assoc.is_none_or(|a| ctx.assoc.builds_owner(c, a))
        }) != PolicyResolution::NotFound
    };
    let name = receiver::strip_generics(hint);
    match receiver::bind(facts, site.caller_qn, name, &admits_any) {
        receiver::Binding::ByName => None,
        receiver::Binding::Decline => Some(rule(hint, None, false)),
        receiver::Binding::Path {
            hint: path_hint,
            path,
            fallback,
        } => Some(PathRule {
            fallback,
            ..rule(&path_hint, Some(path), via == CONSTRUCTED_RETURN_TYPE)
        }),
    }
}
