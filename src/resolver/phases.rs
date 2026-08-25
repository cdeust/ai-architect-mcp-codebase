// resolver::phases — running the Stage-3b resolution phases and folding
// their per-phase tallies into the public `ResolutionResult`.
//
// Split out of `resolver/mod.rs` so `resolve_graph` reads as the composition
// root it is (build the index, run the phases, flush, aggregate) and that
// file stays within the §4.1 size cap.

use super::*;

/// One phase's `(resolved, total, unresolved)` outcome, named so the six of
/// them travel as one value instead of eighteen locals.
struct Tally {
    resolved: u64,
    total: u64,
    unresolved: Vec<UnresolvedRef>,
}

impl From<(u64, u64, Vec<UnresolvedRef>)> for Tally {
    /// Names the phase triple every `resolve_*` returns (see `PhaseResult`).
    fn from(phase: (u64, u64, Vec<UnresolvedRef>)) -> Self {
        Tally {
            resolved: phase.0,
            total: phase.1,
            unresolved: phase.2,
        }
    }
}

/// Every resolution phase's tally, in run order.
pub(super) struct PhaseTallies {
    imports: Tally,
    calls: Tally,
    implements: Tally,
    extends: Tally,
    uses: Tally,
    macros: Tally,
}

/// Runs the resolution phases in order against the shared edge buffer.
/// Extracted from `resolve_graph` so that function is the composition root
/// (build the index, run, flush, aggregate) and nothing else.
pub(super) fn run_phases(
    store: &GraphStore,
    idx: &SymbolIndex,
    file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
) -> Result<PhaseTallies, String> {
    let imports = imports::resolve_imports(store, idx, buf)?.into();
    let calls = calls::resolve_calls(store, idx, file_imports, buf)?.into();
    let implements = implements::resolve_implements(store, idx, file_imports, buf)?.into();
    let extends = extends::resolve_extends(store, idx, file_imports, buf)?.into();
    let uses = uses::resolve_uses(store, idx, file_imports, buf)?.into();

    // 3b-v2 Layer 4/5 — macro + stdlib expansion. Lives in resolver_layers
    // so resolver.rs's function surface stays stable for Q8 ground truth.
    // source: stages/stage-3b-v2.md §5.
    //
    // source: issue #28 — macro refs used to contribute only to the
    // numerator (`macro_resolved` folded into `calls_resolved` with no
    // matching denominator), which let `resolution_rate` exceed 1.0.
    // `run_macro_expansion` now returns the same (resolved, total,
    // unresolved) shape as every other phase; its total is folded into
    // `total_refs` by `into_result`.
    let macros = crate::resolver_layers::run_macro_expansion(store, buf, &|qn: &str| {
        determine_caller_label(idx, qn)
    })?
    .into();

    Ok(PhaseTallies {
        imports,
        calls,
        implements,
        extends,
        uses,
        macros,
    })
}

impl PhaseTallies {
    /// Folds the per-phase tallies into the public result. Macro expansion is
    /// counted into the Calls phase on both sides of the ratio (issue #28).
    pub(super) fn into_result(self, start: Instant) -> ResolutionResult {
        let calls_resolved = self.calls.resolved + self.macros.resolved;
        let calls_total = self.calls.total + self.macros.total;

        let total_edges = self.imports.resolved
            + calls_resolved
            + self.implements.resolved
            + self.extends.resolved
            + self.uses.resolved;
        let total_refs = self.imports.total
            + calls_total
            + self.implements.total
            + self.extends.total
            + self.uses.total;

        let mut unresolved = self.imports.unresolved;
        unresolved.extend(self.calls.unresolved);
        unresolved.extend(self.macros.unresolved);
        unresolved.extend(self.implements.unresolved);
        unresolved.extend(self.extends.unresolved);
        unresolved.extend(self.uses.unresolved);

        // invariant: every reference enters `total_refs` exactly once and
        // produces exactly one resolved-or-unresolved outcome. Each phase
        // upholds this locally (see the per-phase postconditions); this
        // asserts it holds in aggregate. source: issue #28 §"resolved +
        // unresolved == total_refs must hold exactly".
        debug_assert_eq!(
            total_edges + unresolved.len() as u64,
            total_refs,
            "resolution accounting invariant violated: total_edges ({total_edges}) + \
             unresolved ({}) != total_refs ({total_refs})",
            unresolved.len()
        );

        ResolutionResult {
            imports_resolved: self.imports.resolved,
            calls_resolved,
            impls_resolved: self.implements.resolved,
            extends_resolved: self.extends.resolved,
            uses_resolved: self.uses.resolved,
            total_edges,
            total_refs,
            unresolved,
            elapsed_ms: start.elapsed().as_millis() as u64,
        }
    }
}
