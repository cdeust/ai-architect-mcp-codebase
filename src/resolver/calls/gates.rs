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
    if site
        .receiver_hint_via
        .starts_with(crate::graph_store::RECEIVER_HINT_VIA_IMPORT_PREFIX)
    {
        // A return type named only by a `use` of a path the indexer could not
        // show to be a crate of this repository (issues #348 and #349): a
        // foreign crate's type of that name would match a repository namesake.
        return Some(PolicyResolution::NotFound);
    }
    let via_return_type = site.receiver_hint_via
        == crate::graph_store::RECEIVER_HINT_VIA_RETURN_TYPE
        || site.receiver_hint_via == crate::graph_store::RECEIVER_HINT_VIA_CONSTRUCTED_RETURN_TYPE;
    if via_return_type && receiver::names_a_type_alias(ctx.idx, site.receiver_hint) {
        // A return type that is an alias names another type; the lookup by
        // last segment would match a namesake (issues #348 and #349).
        return Some(PolicyResolution::NotFound);
    }
    let resolution = if constructed {
        receiver::resolve_local_receiver_in_file(ctx.idx, site.receiver_hint, &m, file_id)
    } else {
        receiver::resolve_local_receiver_bound(ctx.idx, site.receiver_hint, &m, file_id)
    };
    Some(if via_return_type {
        receiver::relabel_as_return_type(resolution)
    } else {
        resolution
    })
}
