// resolver::extends — Stage-3b Phase 4: Extends (supertrait) resolution
//
// Extracted from resolver.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// resolution types/helpers exactly as when this lived in one module.

use super::*;

// ---------------------------------------------------------------------------
// Phase 4: Extends resolution (supertrait)
// source: stages/stage-3b.md §5.4
// ---------------------------------------------------------------------------

/// Resolves the `bases` CSV property on every Struct/Enum/Trait node, looks
/// each base name up in the symbol index, and emits the matching
/// Extends_X_Y edge. Names that resolve to an Import node (cross-file base)
/// are left unresolved — multi-file indexing surfaces those naturally.
///
/// source: Spike B' BUG #9 fix — previously a no-op stub that just counted
/// pre-existing edges. The parser writes bases to the node property; this
/// function does the deferred name→QN resolution that the indexer can't
/// perform (the indexer routes by labels via label_by_qn, but base names
/// aren't QNs yet at insert time).
///
/// source: issue #28 fix — previously inserted each edge directly via
/// `store.insert_edge`, discarding the `Result` (`let _ = ...`) so a failed
/// insert still counted as resolved, and bypassing the `EdgeBuffer` dedup
/// every other edge kind goes through (so a re-run, or two identical base
/// names in one CSV, wrote duplicate physical edges). Now routes through
/// `buf`/`flush` like every other phase: the flush error propagates via
/// `?` in `resolve_graph`, and duplicates collapse the same way Imports/
/// Calls/Uses do.
pub(super) fn resolve_extends(
    store: &GraphStore,
    idx: &SymbolIndex,
    file_imports: &HashMap<String, Vec<String>>,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();

    for &(label, table_self) in &[
        ("Struct", "Extends_Struct_Struct"),
        ("Enum", "Extends_Enum_Enum"),
        ("Trait", "Extends_Trait_Trait"),
    ] {
        let q = format!("MATCH (s:{label}) RETURN s.id, s.bases, s.language");
        let qr = match store.execute_query(&q) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 3 {
                continue;
            }
            let child_qn = &row[0];
            let bases_csv = &row[1];
            if bases_csv.is_empty() || bases_csv == "Null(String)" {
                continue;
            }
            let provider = crate::language_provider::provider_for(&row[2]);
            let ctx = ExtendsContext { idx, file_imports };
            for raw_base in bases_csv.split(',') {
                let raw_base = raw_base.trim();
                if raw_base.is_empty() {
                    continue;
                }
                total += 1;
                let candidate = ExtendsCandidate {
                    provider,
                    label,
                    table_self,
                    child_qn,
                    raw_base,
                };
                let (r, u) = resolve_one_extends_base(&ctx, buf, &candidate);
                resolved += r;
                unresolved.extend(u);
            }
        }
    }

    Ok((resolved, total, unresolved))
}

/// Read-only lookup context shared by every base name resolved for one file's
/// worth of Struct/Enum/Trait nodes — bundles what `resolve_one_extends_base`
/// only ever reads together, per coding-standards.md §4.4 (>4 params is a
/// missing data type). Mirrors `implements.rs`'s `ResolveContext`.
pub(super) struct ExtendsContext<'a> {
    pub(super) idx: &'a SymbolIndex,
    pub(super) file_imports: &'a HashMap<String, Vec<String>>,
}

/// One base-class/interface name candidate being resolved: its owning
/// node's label/self-table/QN, the raw name text, and the language provider
/// for that node's file (issue #216 — needed to classify a base name that
/// resolves to nothing in the corpus as genuinely external, not just
/// missing). Mirrors `implements.rs`'s `ImplementsCandidate`.
pub(super) struct ExtendsCandidate<'a> {
    pub(super) provider: &'a dyn crate::language_provider::LanguageProvider,
    pub(super) label: &'a str,
    pub(super) table_self: &'a str,
    pub(super) child_qn: &'a str,
    pub(super) raw_base: &'a str,
}

/// Resolves one base-class/interface name for one Struct/Enum/Trait node.
/// postcondition: `(1, vec![])` iff the base name matched a corpus symbol
/// and a schema-known rel table exists for the (label, target.label) pair
/// (regardless of `AddOutcome`); `(0, vec![one entry])` otherwise, its
/// `UnresolvedRef.reason` set to `EXTERNAL_UNRESOLVED_REASON` when the name
/// matches a known-external import in the same file (issue #216), or
/// `"no_target_in_corpus"`/`"no_rel_table_for_..."` otherwise.
/// Extracted from `resolve_extends` to keep that function under the §4.2
/// size limit; behavior is identical to the pre-extraction inline loop body
/// except for the external-reason classification.
pub(super) fn resolve_one_extends_base(
    ctx: &ExtendsContext,
    buf: &mut EdgeBuffer,
    candidate: &ExtendsCandidate,
) -> (u64, Vec<UnresolvedRef>) {
    let ExtendsCandidate {
        provider,
        label,
        table_self,
        child_qn,
        raw_base,
    } = *candidate;
    // Look up by last `.`-separated segment so `typing.NamedTuple` resolves
    // on `NamedTuple` if present. Cortex uses `::` in QNs but base names
    // came from source so they may carry `.`.
    let lookup = raw_base.rsplit('.').next().unwrap_or(raw_base);
    let unresolved_reason = || unresolved_base_reason(provider, ctx.file_imports, child_qn, lookup);
    let candidates = match ctx.idx.by_name.get(lookup) {
        Some(v) => v,
        None => {
            return (
                0,
                vec![UnresolvedRef {
                    kind: "Extends".to_string(),
                    from_id: child_qn.to_string(),
                    target_text: raw_base.to_string(),
                    reason: unresolved_reason(),
                }],
            )
        }
    };
    // Prefer same-label matches (Struct→Struct, etc.) over cross-label
    // (Struct→Trait) for symmetry with `table_self`.
    let target = match candidates
        .iter()
        .find(|c| c.label == label)
        .or_else(|| candidates.first())
    {
        Some(t) => t,
        None => {
            return (
                0,
                vec![UnresolvedRef {
                    kind: "Extends".to_string(),
                    from_id: child_qn.to_string(),
                    target_text: raw_base.to_string(),
                    reason: unresolved_reason(),
                }],
            )
        }
    };
    let rel_table = if target.label == label {
        table_self.to_string()
    } else {
        format!("Extends_{label}_{}", target.label)
    };
    if !crate::graph_store::is_known_rel_table(&rel_table) {
        return (
            0,
            vec![UnresolvedRef {
                kind: "Extends".to_string(),
                from_id: child_qn.to_string(),
                target_text: raw_base.to_string(),
                reason: format!("no_rel_table_for_{label}_to_{}", target.label),
            }],
        );
    }
    // Staged through EdgeBuffer like every other resolution phase
    // (bulk_insert_edges at flush) instead of one insert_edge per base.
    // 0.95/"declared-extends" mirrors resolve_one_implements's 0.95/
    // "declared-implements" for the parallel `implements` CSV.
    // source: ADR-4253701 §Decision 2 (levier 2, resolver.rs resolve_extends).
    buf.add(&rel_table, child_qn, &target.id, 0.95, "declared-extends");
    (1, vec![])
}
