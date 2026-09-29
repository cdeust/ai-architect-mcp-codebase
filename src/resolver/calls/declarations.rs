// resolver::calls::declarations: which candidates a call can actually name,
// before any evidence is weighed.
//
// Two scoping steps run on the candidates of a name:
//   - the language's own rules (`visible_candidates`): Rust block-scoped fn
//     items (issue #327), Python bare calls that never name a method (#335);
//   - what each C-family candidate is (`keep_definitions`, issue #400): a
//     `static` function is named only by its own file or through the header it
//     lives in; a header prototype is never a call target, since a body or a
//     macro of the name is what the call reaches. A function-like macro and a
//     body of one name are left to the evidence tiers: they are usually
//     alternatives under exclusive `#if` configurations (FreeRTOS
//     `vQueueAddToRegistry`), and only the build decides which one exists. The facts come from the graph's
//     `body_kind` and `linkage` columns (`graph_store::body_kind`); a graph
//     written before them yields none and nothing changes.
//
// source: ISO/IEC 9899:2018 §6.2.2 (linkage), §6.9.1 (function definitions),
// §6.10.3 (macro replacement).

use std::borrow::Cow;

use super::reason::{Decline, SCOPE_FILE_LOCAL};
use super::*;
use crate::graph_store::body_kind::{CallableFacts, BODY_KIND_PROTOTYPE};

/// Extensions of the files an `#include` copies into its includer, so a
/// `static` function defined there is local to every file that includes it.
const HEADER_EXTENSIONS: [&str; 4] = [".h", ".hh", ".hpp", ".hxx"];

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
) -> Cow<'a, [SymbolEntry]> {
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

/// The candidates left once the ones the call cannot name are dropped, and why
/// the call is declined when none is left (issue #400).
///
/// A `static` candidate of another file is dropped unless that file is a
/// header. Then every prototype is dropped: a prototype names a function whose
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
    let reachable: Vec<SymbolEntry> = candidates
        .iter()
        .filter(|c| !facts.internal.contains(&c.id) || nameable_from(c, caller_file))
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
    let file = extract_file_prefix_or_self(&candidate.qualified_name);
    file == caller_file || HEADER_EXTENSIONS.iter().any(|ext| file.ends_with(ext))
}
