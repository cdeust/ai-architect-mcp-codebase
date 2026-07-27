// resolver::imports — Stage-3b Phase 1: Import resolution
//
// Extracted from resolver.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared
// resolution types/helpers exactly as when this lived in one module.

use super::*;

pub(super) fn resolve_imports(
    store: &GraphStore,
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
) -> PhaseResult {
    let qr = store.execute_query("MATCH (i:Import) RETURN i.id, i.path, i.is_glob, i.language")?;
    let mut resolved = 0u64;
    let mut total = 0u64;
    let mut unresolved = Vec::new();
    // §10.4 — Import nodes whose target was found; is_resolved is flipped to
    // true for these (all others keep the false written at index time).
    let mut resolved_ids: Vec<&str> = Vec::new();

    for row in &qr.rows {
        if row.len() < 4 {
            continue;
        }
        let provider = crate::language_provider::provider_for(&row[3]);
        let (r, t, u) = resolve_one_import(idx, buf, provider, &row[0], &row[1], &row[2]);
        resolved += r;
        total += t;
        if r > 0 {
            resolved_ids.push(&row[0]);
        }
        unresolved.extend(u);
    }
    store.mark_nodes_resolved("Import", &resolved_ids)?;
    Ok((resolved, total, unresolved))
}

/// Resolves a single Import row. Returns (resolved, total, unresolved) with
/// the invariant `resolved + unresolved.len() as u64 == total` holding for
/// every call — a non-glob import always contributes exactly 1 to `total`
/// (it is one reference); a glob import contributes one unit of `total` per
/// matched symbol (each matched symbol is itself the reference a glob
/// import introduces — there is no "unresolved member" of a glob).
fn resolve_one_import(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    provider: &dyn crate::language_provider::LanguageProvider,
    import_id: &str,
    path: &str,
    is_glob_str: &str,
) -> (u64, u64, Vec<UnresolvedRef>) {
    if provider.is_external_import(path) {
        return (
            0,
            1,
            vec![UnresolvedRef {
                kind: "Imports".to_string(),
                from_id: import_id.to_string(),
                target_text: path.to_string(),
                reason: EXTERNAL_UNRESOLVED_REASON.to_string(),
            }],
        );
    }
    let file_id = extract_file_from_import_id(import_id);
    let normalized = provider.normalize_import_path(path).to_string();
    let is_glob = is_glob_str == "true" || is_glob_str == "True";

    if is_glob {
        let count = resolve_glob_import(idx, buf, &file_id, &normalized);
        return (count, count, vec![]);
    }
    match resolve_single_import(idx, buf, provider, &file_id, &normalized) {
        Ok(count) => (count, 1, vec![]),
        Err(reason) => (
            0,
            1,
            vec![UnresolvedRef {
                kind: "Imports".to_string(),
                from_id: import_id.to_string(),
                target_text: path.to_string(),
                reason: reason.to_string(),
            }],
        ),
    }
}

/// Resolves one non-glob import to its target symbol.
/// postcondition: `Ok(1)` iff a target was found (regardless of whether the
/// edge is new, already persisted, or a within-run duplicate — see
/// `AddOutcome`); `Err(reason)` iff no edge could be produced for this ref.
fn resolve_single_import(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    provider: &dyn crate::language_provider::LanguageProvider,
    file_id: &str,
    normalized_path: &str,
) -> Result<u64, &'static str> {
    let last_segment = provider.import_last_segment(normalized_path);
    let candidates = idx
        .by_name
        .get(last_segment)
        .ok_or("no target found in graph")?;
    // The import path itself is the disambiguating evidence: it plays the
    // role of an "import in scope" whose suffix should match exactly one
    // candidate's qualified name. source: issue #30 — single ambiguity
    // policy shared with resolve_single_call's qualified path.
    let import_hint = [normalized_path.to_string()];
    let ctx = PolicyContext {
        imports_in_scope: &import_hint,
        caller_file: file_id,
        caller_package: None,
    };
    // resolve() leaves a genuinely ambiguous import (no evidence
    // distinguishes the candidates) unresolved rather than guessed — see
    // resolve_single_call's doc comment for why.
    let (entry, evidence, conf) = match ambiguity_policy::resolve(candidates, &ctx) {
        PolicyResolution::Resolved {
            target,
            evidence,
            confidence,
        } => (target, evidence, confidence),
        PolicyResolution::NotFound | PolicyResolution::Ambiguous { .. } => {
            return Err("no target found in graph")
        }
    };
    let table = format!("Imports_File_{}", entry.label);
    if !check_known_rel_table(&table, file_id, &entry.id) {
        return Err("unknown rel table for import target");
    }
    buf.add(
        &table,
        file_id,
        &entry.id,
        conf,
        ambiguity_policy::resolution_label(evidence),
    );
    Ok(1)
}

/// Resolves a glob import (`use module::*`) to every symbol the index
/// knows is a direct child of `module_path`. Each matched symbol is one
/// resolved reference; unlike a single import there is no "not found"
/// case per symbol — the module's members are enumerated from the graph
/// itself, so every candidate considered here is, by construction, real.
pub(super) fn resolve_glob_import(
    idx: &SymbolIndex,
    buf: &mut EdgeBuffer,
    file_id: &str,
    module_path: &str,
) -> u64 {
    // Non-termination fix (2026-07-04): this used to scan idx.by_qn (every
    // symbol in the whole graph) for EVERY glob import, i.e.
    // O(glob_imports * total_symbols). On a repo with a vendored dependency
    // tree (e.g. cortex-viz's 503MB .venv indexed via include_dependencies),
    // total_symbols is huge AND Python packages commonly re-export via
    // `from .submodule import *` inside __init__.py, so glob_imports also
    // scales with corpus size — the product is quadratic in corpus size.
    // Measured: 2_000 glob imports x 100_000 symbols took ~1.3s with the
    // linear-scan version (see bench_glob_import_scaling, "before" run,
    // commit message for the exact numbers). by_parent_module groups
    // symbols by parent qualified-name once (O(total_symbols) at index
    // build time), turning each glob import's cost into O(matches)
    // instead of O(total_symbols).
    let Some(candidates) = idx.by_parent_module.get(module_path) else {
        return 0;
    };
    let mut count = 0u64;
    for entry in candidates {
        let table = format!("Imports_File_{}", entry.label);
        if !check_known_rel_table(&table, file_id, &entry.id) {
            continue;
        }
        buf.add(&table, file_id, &entry.id, 0.9, "import-scope-lookup");
        count += 1;
    }
    count
}
