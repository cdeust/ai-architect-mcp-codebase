// resolver::calls::declarations: which candidates a call can actually name,
// before any evidence is weighed.
//
// Two scoping steps run on the candidates of a name: first the language's
// own rules (`candidate_scope::visible_candidates`, issues #327 and #335),
// then what each C-family candidate is (`keep_definitions`, issue #400): a
// `static` function is named only by its own file or through the header it
// lives in; a header prototype is never a call target, since a body or a macro
// of the name is what the call reaches. A function-like macro and a body of one
// name are left to the evidence tiers: they are usually alternatives under
// exclusive `#if` configurations (FreeRTOS `vQueueAddToRegistry`), and only
// the build decides which one exists. The facts come from the graph's
// `body_kind` and `linkage` columns (`graph_store::body_kind`); a graph written
// before them yields none and nothing changes.
//
// source: ISO/IEC 9899:2018 §6.2.2 (linkage), §6.9.1 (function definitions),
// §6.10.3 (macro replacement).

use std::borrow::Cow;

use super::candidate_scope::visible_candidates;
use super::reason::{Decline, SCOPE_FILE_LOCAL, SCOPE_VARIANT_GUARD};
use super::*;
use crate::graph_store::body_kind::{CallableFacts, BODY_KIND_PROTOTYPE};

/// Extensions of the files an `#include` copies into its includer, so a
/// `static` function defined there is local to every file that includes it.
const HEADER_EXTENSIONS: [&str; 4] = [".h", ".hh", ".hpp", ".hxx"];

/// The candidates a call can name once every scoping step has run, and why
/// the call is declined when none is left: the language's rules
/// (`visible_candidates`), what each C candidate is (`keep_definitions`,
/// #400), then the variant guard (#393). `None` when the repository holds
/// nothing of the name `last`.
pub(super) fn named_candidates<'a>(
    ctx: &ResolveContext<'a>,
    site: &CallSite,
    file_id: &str,
    last: &str,
) -> Option<(Cow<'a, [SymbolEntry]>, Option<Decline>)> {
    let candidates = ctx.idx.by_name.get(last)?;
    let candidates = visible_candidates(ctx, site, candidates, last != site.callee);
    let (candidates, declined) = keep_definitions(ctx.callables, candidates, file_id);
    let visible = candidates.len();
    let candidates = variant_guard::drop_struct_targets(ctx, site.callee, candidates);
    // The variant guard refused every candidate left (issue #393).
    let guarded = declined
        .or((visible > 0 && candidates.is_empty()).then_some(Decline::Scope(SCOPE_VARIANT_GUARD)));
    Some((candidates, guarded))
}

/// The candidates left once the ones the call cannot name are dropped, and why
/// the call is declined when none is left (issue #400).
///
/// A `static` candidate of another file is dropped unless that file is a
/// header. When the caller's own file declares the name `static`, only that
/// file's candidates and the headers' `static` ones remain: the file names its
/// own entity, never an external namesake. Then every prototype is dropped: a prototype names a function whose
/// body or macro is elsewhere, never a target of its own. A name only declared
/// is declined.
///
/// postcondition: never adds a candidate; returns the input untouched when the
/// graph holds no facts or no candidate carries one.
pub(super) fn keep_definitions<'a>(
    facts: &CallableFacts,
    candidates: Cow<'a, [SymbolEntry]>,
    caller_file: &str,
) -> (Cow<'a, [SymbolEntry]>, Option<Decline>) {
    let touched =
        |c: &SymbolEntry| facts.internal.contains(&c.id) || facts.non_body.contains_key(&c.id);
    if !candidates.iter().any(touched) {
        return (candidates, None);
    }
    let internal = |c: &SymbolEntry| facts.internal.contains(&c.id);
    // A file that declares the name `static` names its own entity and no
    // other (§6.2.2p3): an external namesake elsewhere is never the target,
    // even when the file's own definition is missing from the graph.
    let own_internal = candidates
        .iter()
        .any(|c| internal(c) && candidate_file(c) == caller_file);
    let reachable: Vec<SymbolEntry> = candidates
        .iter()
        .filter(|c| {
            if own_internal {
                candidate_file(c) == caller_file || (internal(c) && nameable_from(c, caller_file))
            } else {
                !internal(c) || nameable_from(c, caller_file)
            }
        })
        .cloned()
        .collect();
    if reachable.is_empty() {
        return (
            Cow::Owned(reachable),
            Some(Decline::Scope(SCOPE_FILE_LOCAL)),
        );
    }
    let prototype = |c: &SymbolEntry| {
        facts.non_body.get(&c.id).map(String::as_str) == Some(BODY_KIND_PROTOTYPE)
    };
    let kept: Vec<SymbolEntry> = reachable.into_iter().filter(|c| !prototype(c)).collect();
    let decline = kept.is_empty().then_some(Decline::DeclarationOnly);
    (Cow::Owned(kept), decline)
}

/// True when the file holding `candidate` is the caller's, or a header the
/// caller's file can include.
fn nameable_from(candidate: &SymbolEntry, caller_file: &str) -> bool {
    let file = candidate_file(candidate);
    file == caller_file || HEADER_EXTENSIONS.iter().any(|ext| file.ends_with(ext))
}

/// The file that holds `candidate`.
fn candidate_file(candidate: &SymbolEntry) -> String {
    extract_file_prefix_or_self(&candidate.qualified_name)
}
