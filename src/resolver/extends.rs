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
        let q = format!("MATCH (s:{label}) RETURN s.id, s.bases");
        let qr = match store.execute_query(&q) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 2 {
                continue;
            }
            let child_qn = &row[0];
            let bases_csv = &row[1];
            if bases_csv.is_empty() || bases_csv == "Null(String)" {
                continue;
            }
            for raw_base in bases_csv.split(',') {
                let raw_base = raw_base.trim();
                if raw_base.is_empty() {
                    continue;
                }
                total += 1;
                let (r, u) =
                    resolve_one_extends_base(idx, buf, label, table_self, child_qn, raw_base);
                resolved += r;
                unresolved.extend(u);
            }
        }
    }

    Ok((resolved, total, unresolved))
}

/// Resolves one base-class/interface name for one Struct/Enum/Trait node.
/// postcondition: `(1, vec![])` iff the base name matched a corpus symbol
/// and a schema-known rel table exists for the (label, target.label) pair
/// (regardless of `AddOutcome`); `(0, vec![one entry])` otherwise.
/// Extracted from `resolve_extends` to keep that function under the §4.2
/// size limit; behavior is identical to the pre-extraction inline loop body.
pub(super) fn resolve_one_extends_base(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    label: &str,
    table_self: &str,
    child_qn: &str,
    raw_base: &str,
) -> (u64, Vec<UnresolvedRef>) {
    // Look up by last `.`-separated segment so `typing.NamedTuple` resolves
    // on `NamedTuple` if present. Cortex uses `::` in QNs but base names
    // came from source so they may carry `.`.
    let lookup = raw_base.rsplit('.').next().unwrap_or(raw_base);
    let candidates = match idx.by_name.get(lookup) {
        Some(v) => v,
        None => {
            return (
                0,
                vec![UnresolvedRef {
                    kind: "Extends".to_string(),
                    from_id: child_qn.to_string(),
                    target_text: raw_base.to_string(),
                    reason: "no_target_in_corpus".to_string(),
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
                    reason: "no_target_in_corpus".to_string(),
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
